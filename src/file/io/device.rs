// console 设备选择层 — 统一设备表（合并原 sink.rs 输出选择层 + source.rs 输入选择层）
//
// 一张设备表，一条目双视图：每个 console 设备（UART）同时携带输出句柄
// （writer）与输入缓冲（buffer），**preferred 单一**——读/写天然同设备，
// 注册一次（Linux tty 设备读写一体的对应物，区别于 Linux 把输出侧
// console_list 与输入侧 tty_buffer 分开）。
//
// 本层回答两个问题：
//   - "输出打向哪个设备" → preferred_writer（表空 → None，由 console 决策回落 sbi）
//   - "输入从哪个设备来" → preferred_buffer（表空 → None）
//
// 职责：
//   - 维护设备目录（动态 register；unregister/select 预留）
//   - 维护 preferred（单一索引）：首个注册自动固化，显式 select 后不再自动改
//   - 按名查询（find_writer），供调试显式路由（tprintln!）
//
// "表空即早期"：console 设备注册全部需要 allocator（io::uart::register 的
// Box::leak/format!），boot 早期（allocator 未就绪）表必然为空——preferred_writer
// 返回 None，由 console 决策层回落 sbi 无锁直写（对应 Linux earlycon → 正式
// console）。无显式阶段状态机（Early/Ready 的职责由"表空"表达）。
//
// 无锁输出（panic / lockdep）不经本层——调用方直接 `sbi::mprintln!`
// （M-mode 直写，见 src/sbi/mod.rs），本层不再持有任何 SBI 输出路径。
//
// 设备来源复用能力契约层：UART 驱动 probe 时经 uart::register 联动注册，
// 本层不重新发现设备。
//
// 句柄模型：设备表存 NonNull（可变写句柄），对外提供 `&mut` 视图——唯一
// 写者由 console.rs 的 OUT_LOCK（关中断）保证；输入缓冲为共享 `&`（pop
// 自带内部锁，读侧无需外部互斥）。

use alloc::vec::Vec;
use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::lock::SpinLock;

// ── 输入缓冲 ─────────────────────────────────────────────

/// 输入缓冲容量（字节）— 定长环形，满时丢弃新字符（保旧）。
pub const INPUT_BUFFER_CAPACITY: usize = 256;

/// 输入缓冲 — SpinLock 保护定长环形缓冲。
///
/// push 由中断上下文（InputHandler）调用；pop 由读取侧（console::read /
/// devfs File）调用。满时丢弃新字符（终端输入场景保旧更合理）。
pub struct InputBuffer {
    inner: SpinLock<Ring>,
}

struct Ring {
    data: [u8; INPUT_BUFFER_CAPACITY],
    /// 下一个写入位置（环形索引）
    head: usize,
    /// 已缓冲字节数
    len: usize,
}

impl InputBuffer {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(Ring {
                data: [0; INPUT_BUFFER_CAPACITY],
                head: 0,
                len: 0,
            }),
        }
    }

    /// 入队一个字节（满则丢弃新字符）。
    ///
    /// 返回是否发生"空 → 非空"转变——调用方（InputHandler）据此决定是否
    /// 唤醒输入等待者（等待者只在缓冲空时阻塞，非空时 pop 直接成功）。
    pub fn push(&self, c: u8) -> bool {
        let mut ring = self.inner.lock();
        let was_empty = ring.len == 0;
        if ring.len == INPUT_BUFFER_CAPACITY {
            return false; // 满：丢弃新字符
        }
        let head = ring.head;
        ring.data[head] = c;
        ring.head = (head + 1) % INPUT_BUFFER_CAPACITY;
        ring.len += 1;
        was_empty
    }

    /// 出队一个字节（非阻塞；空返回 `None`）。
    pub fn pop(&self) -> Option<u8> {
        let mut ring = self.inner.lock();
        if ring.len == 0 {
            return None;
        }
        let tail = (ring.head + INPUT_BUFFER_CAPACITY - ring.len) % INPUT_BUFFER_CAPACITY;
        let c = ring.data[tail];
        ring.len -= 1;
        Some(c)
    }

    /// 当前缓冲字节数。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().len
    }

    /// 是否为空。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().len == 0
    }
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ── 设备条目 ─────────────────────────────────────────────

/// console 设备 — 注册表条目：身份 + 读写双视图（各可选）。
///
/// 同一设备（UART）读写一体：writer 为输出句柄（可变；唯一写者由
/// print.rs 的 OUT_LOCK 保证），buffer 为输入缓冲（共享，pop 内部锁）。
/// 支持仅输出（writer Some）或仅输入设备（buffer None）。
pub struct ConsoleDevice {
    /// 设备身份（按名 select/find/unregister）
    pub name: &'static str,
    /// 输出句柄（可选；仅输入设备为 None）
    pub(crate) writer: Option<NonNull<dyn ByteWrite>>,
    /// 输入缓冲（可选；sbi 为 None——输入无兜底）
    pub(crate) buffer: Option<&'static InputBuffer>,
}

impl ConsoleDevice {
    /// 构造设备条目。
    ///
    /// `writer`/`buffer` 均为可选：输出-only 设备只传 writer；
    /// 读写一体设备（UART）两者都传。
    pub fn new(
        name: &'static str,
        writer: Option<&'static dyn ByteWrite>,
        buffer: Option<&'static InputBuffer>,
    ) -> Self {
        ConsoleDevice {
            name,
            writer: writer.map(|w| {
                // SAFETY: writer 指向 'static 实例（UART writer 泄漏），非空。
                unsafe {
                    NonNull::new_unchecked(w as *const dyn ByteWrite as *mut dyn ByteWrite)
                }
            }),
            buffer,
        }
    }
}

/// 设备选择错误
///
/// NameTaken 的载荷当前无读取方（register 的调用方丢弃错误），保留以携带诊断信息。
#[allow(dead_code)]
#[derive(Debug)]
pub enum DeviceError {
    /// 注册时名字已存在
    NameTaken(&'static str),
    /// select/find/unregister 时名字不存在
    UnknownDevice(&'static str),
}

// ── 字节级输出能力 ─────────────────────────────────────────

/// 字节级输出 — `fmt::Write` 受 UTF-8 约束（`write_str(&str)` 需合法 UTF-8），
/// 无法直写任意字节；终端/console 输出是字节流（`\n` → `\r\n`），故引入
/// 字节写者。实现者：`UartWriter`（io::uart）。
///
/// `preferred_writer()` 返回 `&mut dyn ByteWrite`；`print::write_bytes` 走本
/// trait 逐字节输出（`Stdout::write` 的字节语义由此承载）。
pub trait ByteWrite: fmt::Write {
    /// 逐字节写出 — 实现者负责 `\n` → `\r\n` 等终端转换。
    fn write_bytes(&mut self, bytes: &[u8]);
}

// ── 注册表 ──────────────────────────────────────────────

/// 设备注册表 — 按注册顺序排列（无 sbi 常驻项；表空 = boot 早期）。
static DEVICES: SpinLock<Vec<ConsoleDevice>> = SpinLock::new(Vec::new());

/// preferred 设备索引（PREFERRED_INVALID = 未固化 → 取首个）
const PREFERRED_INVALID: usize = usize::MAX;
static PREFERRED: AtomicUsize = AtomicUsize::new(PREFERRED_INVALID);

/// preferred 是否已固化（首个注册自动固化，或显式 select）——固化后
/// 后续 register 不再自动改默认目标（Linux preferred_console 语义）。
static PREFERRED_PINNED: AtomicBool = AtomicBool::new(false);

// ── 公共 API ─────────────────────────────────────────────

/// 注册 console 设备 — 首个注册（sbi 之外）自动成为 preferred。
///
/// UART 驱动 probe 时经 `uart::register` 联动调用；名字冲突返回
/// [`DeviceError::NameTaken`]。
pub fn register(dev: ConsoleDevice) -> Result<(), DeviceError> {
    let mut list = DEVICES.lock();
    if list.iter().any(|d| d.name == dev.name) {
        return Err(DeviceError::NameTaken(dev.name));
    }
    let idx = list.len();
    list.push(dev);
    // 首次注册自动固化 preferred；此后（显式 select 过）不再自动改
    if !PREFERRED_PINNED.load(Ordering::Relaxed) {
        PREFERRED.store(idx, Ordering::Relaxed);
        PREFERRED_PINNED.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// 注销打印设备 — preferred 被移除时回落 sbi。sbi 常驻不可注销。
///
/// 预留 API：当前无调用方（设备只增不删），保持 Linux unregister_console 语义。
#[allow(dead_code)]
pub fn unregister(name: &'static str) -> Result<(), DeviceError> {
    let mut list = DEVICES.lock();
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(DeviceError::UnknownDevice(name))?;
    list.remove(idx);
    // preferred 索引修正：被移除位置之后整体前移
    let pref = PREFERRED.load(Ordering::Relaxed);
    if pref == idx {
        // 移除的是 preferred → 回落首个设备（0），允许后续 register 重新自动选择
        PREFERRED.store(0, Ordering::Relaxed);
        PREFERRED_PINNED.store(false, Ordering::Relaxed);
    } else if pref > idx {
        PREFERRED.store(pref - 1, Ordering::Relaxed);
    }
    Ok(())
}

/// 显式切换 preferred — 固化后 register 不再自动改默认。
///
/// 预留 API：当前无调用方，保持 Linux preferred_console 语义。
#[allow(dead_code)]
pub fn select(name: &'static str) -> Result<(), DeviceError> {
    let list = DEVICES.lock();
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(DeviceError::UnknownDevice(name))?;
    PREFERRED.store(idx, Ordering::Relaxed);
    PREFERRED_PINNED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 当前 preferred 输出设备的可变句柄 — 表空（boot 早期）返回 `None`，
/// 由 console 决策层回落 sbi 无锁直写。
///
/// # Safety
///
/// 返回 'static `&mut`。唯一写者由调用方（console.rs 持 OUT_LOCK，关中断）
/// 保证；本模块不提供共享 `&` 视图，避免与写者别名。
pub(crate) fn preferred_writer() -> Option<&'static mut dyn ByteWrite> {
    let list = DEVICES.lock();
    if list.is_empty() {
        return None;
    }
    let idx = PREFERRED.load(Ordering::Relaxed).min(list.len() - 1);
    let dev = &list[idx];
    // SAFETY: 调用方（console.rs 持 OUT_LOCK）保证唯一写者。
    dev.writer.map(|w| unsafe { &mut *w.as_ptr() })
}

/// 当前 preferred 输入缓冲（首个注册的输入设备；未注册 → None）。
///
/// 输入侧无 SBI 类常驻兜底，返回 `Option`——调用方（console::read::read）
/// 无设备时返回错误，避免永久阻塞。
pub fn preferred_buffer() -> Option<&'static InputBuffer> {
    let list = DEVICES.lock();
    let idx = PREFERRED.load(Ordering::Relaxed);
    // `dev.buffer` 是 'static 引用（register 时 Box::leak），拷贝即可脱离锁生命周期。
    list.get(idx).and_then(|dev| dev.buffer)
}

/// 按名查找输出设备的可变句柄 — 不改变 preferred（调试显式路由用）。
///
/// 零分配纯查询：早期阶段表为空，返回 None（tprintln! 是 boot 后调试路由）。
///
/// # Safety
///
/// 同 [`preferred_writer`]：调用方必须保证唯一写者。
/// 预留：tprint!/tprintln! 的查找路径，当前无调用方。
// clippy::mut_from_ref：`name` 仅作查找键，返回的 &mut 来自受 OUT_LOCK
// 保护的设备表（NonNull 存储），不从不可变参数派生借用——语义非误用。
#[allow(dead_code, clippy::mut_from_ref)]
pub(crate) fn find_writer(name: &'static str) -> Option<&'static mut dyn ByteWrite> {
    let list = DEVICES.lock();
    list.iter().find(|d| d.name == name).and_then(|dev| {
        // SAFETY: 调用方保证唯一写者（print.rs 持 OUT_LOCK）。
        dev.writer.map(|w| unsafe { &mut *w.as_ptr() })
    })
}

/// 已注册的 console 设备数 — 启动日志与诊断用。
pub fn count() -> usize {
    let list = DEVICES.lock();
    list.len()
}
