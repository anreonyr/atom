// 内核日志系统
//
// 提供五级日志宏：error! / warn! / info! / debug! / trace!
//
// 三层过滤：
//   1. 编译期：COMPILE_MAX_LEVEL 以上的调用在宏展开中被移除（零开销）
//   2. 运行时全局：set_max_level() 动态调整，默认 Info
//   3. 运行时模块级：set_module_rules() 按模块路径前缀覆盖全局级别（最长前缀匹配）
//
// 双重输出：
//   - console：S-mode println! 输出，每条日志两行（header 行 + 缩进的 msg 行），
//     header 带 ANSI 颜色（时间戳/模块灰色，级别彩色），模块显示短名
//   - ring buffer：最近 RING_CAP 条日志的消息体快照（纯文本），log_read() 读取；
//     console 未就绪的早期日志不丢失
//
// 死锁安全性：
//   日志输出经 println! → print::S_WRITER，由 writer 锁（关中断）串行化。
//   无论从任务上下文还是中断上下文输出，都不会发生"持锁被同 CPU 中断抢占 →
//   中断路径争同一把锁"的死锁。
//   ring buffer 用 SpinLock（关中断）保护，与 writer 锁各自独立、不嵌套持有。
//   运行时级别用 AtomicU8 无锁读写；时钟源与模块规则表用 OnceLock 写一次读多次。

use core::fmt;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::lock::{OnceLock, SpinLock};

/// 日志级别（按严重程度递增）
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

/// 编译期最高日志级别——高于此级别的调用在宏展开中被编译器移除。
///
/// 发布构建时可改为 `LogLevel::Info` 以完全消除 debug/trace 代码。
pub const COMPILE_MAX_LEVEL: LogLevel = LogLevel::Trace;

/// 运行时最高日志级别（默认 Info：Error + Warn + Info 可见）
static RUNTIME_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// 设置运行时最高日志级别（全局默认；模块级规则可覆盖）
pub fn set_max_level(level: LogLevel) {
    RUNTIME_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// 获取当前运行时最高日志级别
pub fn max_level() -> LogLevel {
    level_from_u8(RUNTIME_LEVEL.load(Ordering::Relaxed))
}

/// 将原始值映射回 LogLevel（值域受 set_max_level/规则表约束，恒合法）
fn level_from_u8(v: u8) -> LogLevel {
    match v {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        3 => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

// ═══════════════════════════════════════════════════════════════════
// 时钟源 — 时间戳来源
// ═══════════════════════════════════════════════════════════════════

/// 单调时钟源 — 内核日志的时间戳来源。
///
/// boot 阶段注册一次，之后每条日志仅一次虚调用（如读 `time` CSR），
/// 避免日志路径做设备表查找。
pub trait Clock: Sync {
    /// 返回当前时刻的时钟计数值（如 mtime/cycle）。
    fn now(&self) -> u64;
}

/// 直接读 `time` CSR 的时钟源（Sstc 扩展，S-mode 可访问）。
///
/// boot 最早即可用，无需任何驱动实例——`init_timestamp` 在驱动初始化前注册，
/// 让早期启动日志（console/驱动就绪前）也带真实时间戳。
pub struct CsrClock;

// SAFETY: 无内部状态，读 CSR 无副作用。
unsafe impl Sync for CsrClock {}

impl Clock for CsrClock {
    fn now(&self) -> u64 {
        let t: u64;
        // SAFETY: `time` CSR 是 Sstc 扩展的只读时间计数器，S-mode 可读。
        unsafe { core::arch::asm!("csrr {}, time", out(reg) t) };
        t
    }
}

/// 全局默认时钟源实例。
pub static CSR_CLOCK: CsrClock = CsrClock;

/// 时钟源（时钟对象 + 频率），init 时注册一次。
struct ClockSource {
    clock: &'static dyn Clock,
    freq: u64,
}

/// 时钟源存储（写一次读多次）。
static CLOCK: OnceLock<ClockSource> = OnceLock::new();

/// 注册时钟源（应在 init 阶段调用一次）。
///
/// `clock` 返回当前 mtime 值，`freq` 为定时器频率 (Hz)。
pub fn init_timestamp(clock: &'static dyn Clock, freq: u64) {
    CLOCK.set(ClockSource { clock, freq }).ok();
}

/// 读取当前 (秒, 微秒)——未注册时钟源时返回 None。
fn read_time() -> Option<(u64, u64)> {
    CLOCK.get().map(|src| {
        let freq = src.freq.max(1);
        let mt = src.clock.now();
        let sec = mt / freq;
        // 余数刻度 → 微秒：r * 1e6 / freq；r < freq 保证结果恒 < 1e6
        let usec = (mt % freq) * 1_000_000 / freq;
        (sec, usec)
    })
}

/// 格式化时间戳显示串（未注册时钟时为 `?.??????`）。
fn fmt_time(sec: u64, usec: u64, registered: bool) -> Buf<24> {
    let mut t = Buf::new();
    if registered {
        let _ = write!(t, "{}.{:06}", sec, usec);
    } else {
        let _ = t.write_str("?.??????");
    }
    t
}

// ═══════════════════════════════════════════════════════════════════
// 模块级过滤 — 按模块路径前缀覆盖全局级别
// ═══════════════════════════════════════════════════════════════════

/// 模块级过滤规则 — 按模块路径前缀控制日志级别。
pub struct ModuleRule {
    /// 模块路径前缀（`module_path!()` 值，如 `"driver::serial"`）
    pub prefix: &'static str,
    /// 该模块下生效的最高日志级别
    pub level: LogLevel,
}

/// 模块级过滤规则表（boot 时设置一次，之后只读——日志路径无锁）。
static MODULE_RULES: OnceLock<&'static [ModuleRule]> = OnceLock::new();

/// 设置模块级过滤规则。
///
/// 规则按最长前缀匹配：日志的 `module_path!()` 命中多条规则时取前缀最长者；
/// 无命中则回落全局 `set_max_level()` 的默认级别。
pub fn set_module_rules(rules: &'static [ModuleRule]) {
    MODULE_RULES.set(rules).ok();
}

/// 计算模块的有效最高级别（模块规则命中取规则值，否则取全局默认）。
fn effective_level(module: &str) -> LogLevel {
    // module_path!() 以 crate 名为前缀（如 "atom::memory::allocator::frame"），
    // 规则前缀与 crate 名无关——匹配前剥离首段。
    let module = module
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or(module);
    let mut best: Option<(usize, u8)> = None;
    if let Some(rules) = MODULE_RULES.get() {
        for rule in rules.iter() {
            if module == rule.prefix
                || module
                    .strip_prefix(rule.prefix)
                    .is_some_and(|rest| rest.starts_with("::"))
            {
                let len = rule.prefix.len();
                if best.map_or_else(|| true, |(bl, _)| len > bl) {
                    best = Some((len, rule.level as u8));
                }
            }
        }
    }
    best.map_or_else(max_level, |(_, l)| level_from_u8(l))
}

// ═══════════════════════════════════════════════════════════════════
// ring buffer — 最近日志快照
// ═══════════════════════════════════════════════════════════════════

/// 环形缓冲容量（日志条数）
const RING_CAP: usize = 128;
/// 单条日志消息体容量（字节）
const MSG_CAP: usize = 256;
/// 模块短名容量（字节）
const MODULE_CAP: usize = 24;
/// 日志读取时的单行缓冲容量（header + 缩进 msg + 换行）
const LINE_CAP: usize = 40 + MSG_CAP + 1;

/// 环形缓冲条目 — 只存消息体与定位信息（纯文本，无颜色、无换行/缩进）。
#[derive(Clone, Copy)]
struct LogEntry {
    level: u8,
    sec: u64,
    usec: u32,
    module_len: u8,
    module: [u8; MODULE_CAP],
    msg_len: u16,
    msg: [u8; MSG_CAP],
}

impl LogEntry {
    const fn empty() -> Self {
        LogEntry {
            level: 0,
            sec: 0,
            usec: 0,
            module_len: 0,
            module: [0; MODULE_CAP],
            msg_len: 0,
            msg: [0; MSG_CAP],
        }
    }

    fn new(level: LogLevel, sec: u64, usec: u64, module: &str, msg: &str) -> Self {
        let mut e = Self::empty();
        e.level = level as u8;
        e.sec = sec;
        e.usec = usec as u32;
        let m = &module.as_bytes()[..module.len().min(MODULE_CAP)];
        e.module[..m.len()].copy_from_slice(m);
        e.module_len = m.len() as u8;
        let b = &msg.as_bytes()[..msg.len().min(MSG_CAP)];
        e.msg[..b.len()].copy_from_slice(b);
        e.msg_len = b.len() as u16;
        e
    }
}

/// 日志环形缓冲 — 最近 RING_CAP 条日志。
struct LogRing {
    entries: [LogEntry; RING_CAP],
    /// 下一个写入位置
    head: usize,
    /// 已存条数（≤ RING_CAP，写满后恒为 RING_CAP）
    count: usize,
}

impl LogRing {
    const fn new() -> Self {
        LogRing {
            entries: [LogEntry::empty(); RING_CAP],
            head: 0,
            count: 0,
        }
    }

    fn push(&mut self, e: LogEntry) {
        self.entries[self.head] = e;
        self.head = (self.head + 1) % RING_CAP;
        if self.count < RING_CAP {
            self.count += 1;
        }
    }

    /// 按写入顺序（最旧 → 最新）迭代。
    fn iter(&self) -> impl Iterator<Item = &LogEntry> + '_ {
        let start = if self.count == RING_CAP { self.head } else { 0 };
        let n = self.count;
        (0..n).map(move |i| &self.entries[(start + i) % RING_CAP])
    }
}

static RING: SpinLock<LogRing> = SpinLock::new(LogRing::new());

/// 级别标签（无颜色，供 log_read 使用）。
fn label_str(level: u8) -> &'static str {
    level_style(level_from_u8(level)).1
}

fn module_of(e: &LogEntry) -> &str {
    core::str::from_utf8(&e.module[..e.module_len as usize]).unwrap_or("?")
}

fn msg_of(e: &LogEntry) -> &str {
    core::str::from_utf8(&e.msg[..e.msg_len as usize]).unwrap_or("?")
}

/// 将环形缓冲中的日志按快照语义拷贝到 `buf`。
///
/// `offset` 为从最旧条目起的字节偏移（由 VFS 维护），返回实际写入字节数。
/// 每条日志输出为两行：header 行 + 缩进 4 空格的 msg 行。
pub fn log_read(offset: usize, buf: &mut [u8]) -> usize {
    let ring = RING.lock();
    let mut out = 0usize;
    let mut pos = 0usize;
    for e in ring.iter() {
        let mut line = Buf::<LINE_CAP>::new();
        let _ = write!(
            line,
            "[{}] ({}) {}.{:06}\n    {}\n",
            label_str(e.level),
            module_of(e),
            e.sec,
            e.usec,
            msg_of(e),
        );
        let len = line.len();
        if pos + len > offset {
            let start = offset.saturating_sub(pos);
            let n = (len - start).min(buf.len() - out);
            buf[out..out + n].copy_from_slice(&line.as_slice()[start..start + n]);
            out += n;
            if out == buf.len() {
                break;
            }
        }
        pos += len;
    }
    out
}

// ═══════════════════════════════════════════════════════════════════
// 格式化辅助
// ═══════════════════════════════════════════════════════════════════

/// 定长栈缓冲 — 兼作 fmt::Write 目标，把日志格式化进无堆缓冲。
struct Buf<const N: usize> {
    data: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Buf {
            data: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.data[..self.len]).unwrap_or("")
    }

    fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    fn len(&self) -> usize {
        self.len
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // 容量不足时静默截断
        let room = N - self.len;
        let n = s.len().min(room);
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// 级别颜色 + 标签（console 使用）。
fn level_style(level: LogLevel) -> (&'static str, &'static str) {
    match level {
        LogLevel::Error => ("\x1b[1;31m", "ERROR"),
        LogLevel::Warn => ("\x1b[1;33m", "WARN"),
        LogLevel::Info => ("\x1b[32m", "INFO"),
        LogLevel::Debug => ("\x1b[36m", "DEBUG"),
        LogLevel::Trace => ("\x1b[35m", "TRACE"),
    }
}

/// 模块路径短名 — `module_path!()` 的最后一段（如 `driver::serial::uart16550` → `uart16550`）。
fn short_module(module: &str) -> &str {
    module.rsplit("::").next().unwrap_or(module)
}

/// 文件路径 basename — `file!()` 的最后一段（如 `/…/src/log.rs` → `log.rs`）。
fn short_file(file: &str) -> &str {
    file.rsplit('/').next().unwrap_or(file)
}

// ═══════════════════════════════════════════════════════════════════
// 核心输出
// ═══════════════════════════════════════════════════════════════════

/// 核心日志输出（由宏调用，不应直接使用）
#[doc(hidden)]
pub fn _log(level: LogLevel, args: core::fmt::Arguments, module: &str, file: &str, line: u32) {
    // 运行时级别检查（全局默认 + 模块级覆盖）
    if (level as u8) > (effective_level(module) as u8) {
        return;
    }

    // 时间戳
    let (sec, usec) = read_time().unwrap_or((0, 0));
    let time = fmt_time(sec, usec, CLOCK.is_initialized());

    // 模块短名与文件名 basename
    let short = short_module(module);
    let fname = short_file(file);

    // 消息体格式化（纯文本，截断到 MSG_CAP）
    let mut body = Buf::<MSG_CAP>::new();
    let _ = write!(body, "{}", args);

    // 入 ring buffer（无条件：早期日志不因 console 未就绪而丢失）
    RING.lock()
        .push(LogEntry::new(level, sec, usec, short, body.as_str()));

    // console 输出：header 行（[级别] (模块) 时间）+ 缩进的 msg 行，一次 println 原子输出
    let (color, label) = level_style(level);
    let reset = "\x1b[0m";
    match level {
        LogLevel::Debug | LogLevel::Trace => {
            // [DEBUG] (clint) clint.rs:53 0.077284
            println!(
                "[{}{}{}] (\x1b[90m{}\x1b[0m) \x1b[90m{}:{} {}\x1b[0m\n    {}",
                color,
                label,
                reset,
                short,
                fname,
                line,
                time.as_str(),
                body.as_str(),
            );
        }
        _ => {
            // [INFO] (atom) 0.074345
            println!(
                "[{}{}{}] (\x1b[90m{}\x1b[0m) \x1b[90m{}\x1b[0m\n    {}",
                color,
                label,
                reset,
                short,
                time.as_str(),
                body.as_str(),
            );
        }
    }
}

/// 条件日志（内部使用，通过具体级别宏调用）
///
/// 编译期常量比较使编译器在低级别构建中优化掉高于 COMPILE_MAX_LEVEL 的调用。
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        if ($level as u8) <= ($crate::log::COMPILE_MAX_LEVEL as u8) {
            $crate::log::_log(
                $level,
                format_args!($($arg)*),
                module_path!(),
                file!(),
                line!(),
            );
        }
    }};
}

/// 错误日志——最高严重级别
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Error, $($arg)*) };
}

/// 警告日志
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Warn, $($arg)*) };
}

/// 信息日志（默认运行时可见）
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Info, $($arg)*) };
}

/// 调试日志（默认运行时不可见，需 set_max_level(Debug) 启用）
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Debug, $($arg)*) };
}

/// 跟踪日志——最低级别，最详细
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Trace, $($arg)*) };
}
