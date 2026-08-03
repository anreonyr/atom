// 日志模块 — 五级日志宏
//
// 本模块是"格式化层"而非输出系统：负责时间戳/级别/模块/颜色/ring buffer
// 快照的格式化，字节输出全部委托 print.rs / sink.rs 的通道：
//   - console 输出：println! → print::write（带锁，OUT_LOCK + sink::current()）
//
// 本文件只留输出编排（_log/log_line）与对外 API 重新导出，
// 其余按职责拆分子模块：
//   - filter.rs   级别（LogLevel）+ 三层过滤（编译期/全局/模块级）
//   - clock.rs    时钟源（Clock/CSR_CLOCK/read_time/fmt_time）
//   - record.rs   日志消息结构（LogMessage owned 定长，console 渲染 / ring 存储同一结构）
//   - palette.rs  ANSI 调色板 + 级别展示元数据（颜色唯一来源）
//   - buf.rs      定长栈缓冲（无堆格式化目标，fmt::Write 实现）
//   - ring.rs     最近日志环形快照（LogMessage/LogRing/log_read/log_seq_range）
//   - macros.rs   日志宏（log!/error!/warn!/info!/debug!/trace!）
//
// 双重输出：
//   - console：S-mode println! 输出，每条日志两行（header 行 + 缩进的 msg 行），
//     header 带 ANSI 颜色（时间戳/模块灰色，级别彩色），模块显示短名；
//     显示级别 ≤ console_level（默认 Info，Linux console_loglevel 对应物）
//   - ring buffer：最近 RING_CAP 条日志的消息体快照（纯文本，带 seq 序号），
//     log_read() 读取；console 未就绪的早期日志不丢失，
//     且 /dev/log 可读到 console 未显示的细日志（ring 与 console 级别分离）
//
// 死锁安全性：
//   日志输出经 println! → print::write，由 OUT_LOCK（关中断）串行化。
//   无论从任务上下文还是中断上下文输出，都不会发生"持锁被同 CPU 中断抢占 →
//   中断路径争同一把锁"的死锁。
//   ring buffer 用 SpinLock（关中断）保护，与 OUT_LOCK 各自独立、不嵌套持有。
//   运行时级别用 AtomicU8 无锁读写；时钟源与模块规则表用 OnceLock 写一次读多次。

use core::fmt::Write as _;

#[macro_use]
mod macros;

mod buf;
mod clock;
mod filter;
mod palette;
mod record;
mod ring;

// ── 对外 API 重新导出（宏经 $crate::log:: 引用同一路径）──
pub use clock::{set_clock_source, Clock, CSR_CLOCK};
// set_console_level/console_level 为预留 API（当前无调用方），allow 保留导出
#[allow(unused_imports)]
pub use filter::{
    console_level, max_level, set_console_level, set_max_level, set_module_rules, LogLevel,
    ModuleRule, COMPILE_MAX_LEVEL,
};
#[allow(unused_imports)]
pub use ring::{log_read, log_seq_range};

use buf::Buf;
use clock::{fmt_time, read_time};
use filter::{console_shows, effective_level};
use palette::{level_style, GRAY, RESET};
use record::{LogMessage, Timestamp};
use ring::RING;

/// 模块路径短名 — `module_path!()` 的最后一段（如 `driver::serial::uart16550` → `uart16550`）。
fn short_module(module: &str) -> &str {
    module.rsplit("::").next().unwrap_or(module)
}

/// 文件路径 basename — `file!()` 的最后一段（如 `/…/src/log.rs` → `log.rs`）。
fn short_file(file: &str) -> &str {
    file.rsplit('/').next().unwrap_or(file)
}

/// 核心日志输出（由宏调用，不应直接使用）
///
/// 运行时级别检查（全局默认 + 模块级覆盖）通过后委托 [`log_line`]。
#[doc(hidden)]
pub fn _log(
    level: LogLevel,
    args: core::fmt::Arguments,
    module: &str,
    file: &'static str,
    line: u32,
) {
    // 运行时级别检查（全局默认 + 模块级覆盖）
    if (level as u8) > (effective_level(module) as u8) {
        return;
    }
    log_line(level, args, module, file, line);
}

/// 日志行格式化 + 双输出（ring buffer + console）— [`_log`] 入口。
///
/// `file` 恒为 `file!()` 字面量（'static），供 `LogMessage.loc` 存入 ring 快照。
fn log_line(
    level: LogLevel,
    args: core::fmt::Arguments,
    module: &str,
    file: &'static str,
    line: u32,
) {
    // 时间戳（一次读取，console 与 ring 共用同一时刻）
    let t = read_time();

    // 组装统一消息结构（owned 定长）：消息体直接格式化进内部缓冲
    let mut m = LogMessage::new(
        level,
        t.unwrap_or(Timestamp::new(0)),
        short_module(module),
        matches!(level, LogLevel::Debug | LogLevel::Trace).then(|| (short_file(file), line)),
    );
    let _ = write!(m.msg_mut(), "{}", args);

    // console 输出：级别 > console_level 时跳过（ring 已记录，可经 /dev/log 读）。
    // 先渲染 console（借用 &m），再入 ring（move m）——顺序不可换。
    if console_shows(m.level) {
        // console 行：header（[级别] (模块) 时间）+ 缩进的 msg 行，一次 println 原子输出。
        // 定位段（file:line）仅 Debug/Trace 级别附带：
        //   [DEBUG] (clint) clint.rs:53 0.077284
        //   [INFO]  (atom) 0.074345
        let (color, label) = level_style(m.level);
        let mut loc = Buf::<64>::new(); // file:line 定位段（空 = 不带）
        if let Some((f, l)) = m.loc {
            let _ = write!(loc, "{}:{} ", f, l);
        }
        println!(
            "{color}[{label}]{reset}\t({gray}{short}{reset}) {gray}{loc}{time}{reset}\n\t{gray}{body}{reset}",
            color = color,
            label = label,
            reset = RESET,
            gray = GRAY,
            short = m.module(),
            loc = loc.as_str(),
            time = fmt_time(t).as_str(),
            body = m.msg(),
        );
    }

    // 入 ring buffer（无条件：早期日志不因 console 未就绪而丢失；console 跳过也记录）
    RING.lock().push(m);
}
