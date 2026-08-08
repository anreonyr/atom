// console 设备选择层 — 统一设备表（合并原 sink.rs 输出选择层 + source.rs 输入选择层）
//
// 一张设备表，一条目双视图：每个 console 设备（UART）同时携带输出句柄
// （writer）与输入缓冲（buffer），**preferred 单一**——读/写天然同设备，
// 注册一次（Linux tty 设备读写一体的对应物，区别于 Linux 把输出侧
// console_list 与输入侧 tty_buffer 分开）。
//
// 本层回答两个问题：
//   - "输出打向哪个设备" → preferred_writer（sbi 兜底：表恒非空）
//   - "输入从哪个设备来" → preferred_buffer（无兜底：Option）
//
// 职责：
//   - 维护设备目录（动态 register；unregister/select 预留）
//   - 维护 preferred（单一索引）：首个注册自动固化，显式 select 后不再自动改
//   - 按名查询（find_writer），供调试显式路由（tprintln!）
//   - sbi 常驻设备（name = "sbi"，writer Some / buffer None——输入无兜底）
//
// 早期阶段（显式状态机）：boot 早期 allocator 未就绪，输出路径不得碰堆——
// Early 阶段 preferred_writer 静态直写 SBI（不查表、不拿注册表锁），首个
// register 切 Ready 后才走注册表。对应 Linux earlycon → 正式 console。
// 输入侧无早期消费者（preferred_buffer 表空 → None）。
//
// panic 路径经静态 SBI_WRITER（mprint!/mprintln!）无锁直写（不查表、不拿锁），
// 保证任意持锁状态崩溃仍能输出，与 Phase 无关。
//
// 设备来源复用能力契约层：UART 驱动 probe 时经 uart::register 联动注册，
// 本层不重新发现设备。
//
// 句柄模型：设备表存 NonNull（可变写句柄），对外提供 `&mut` 视图——唯一
// 写者由 print.rs 的 OUT_LOCK（关中断）保证；输入缓冲为共享 `&`（pop 自带
// 内部锁，读侧无需外部互斥）。

use alloc::vec::Vec;
use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

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
/// 支持仅输出（sbi：buffer None）或仅输入设备（writer None）。
pub struct ConsoleDevice {
    /// 设备身份（按名 select/find/unregister）
    pub name: &'static str,
    /// 输出句柄（可选；sbi 恒有，仅输入设备为 None）
    pub(crate) writer: Option<NonNull<dyn fmt::Write>>,
    /// 输入缓冲（可选；sbi 为 None——输入无兜底）
    pub(crate) buffer: Option<&'static InputBuffer>,
}

impl ConsoleDevice {
    /// 构造设备条目。
    ///
    /// `writer`/`buffer` 均为可选：输出-only 设备（sbi）传 writer 不传
    /// buffer；读写一体设备（UART）两者都传。
    pub fn new(
        name: &'static str,
        writer: Option<&'static dyn fmt::Write>,
        buffer: Option<&'static InputBuffer>,
    ) -> Self {
        ConsoleDevice {
            name,
            writer: writer.map(|w| {
                // SAFETY: writer 指向 'static 实例（UART writer 泄漏或 SBI 静态），非空。
                unsafe {
                    NonNull::new_unchecked(w as *const dyn fmt::Write as *mut dyn fmt::Write)
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
    /// 注册时名字已存在（或注销常驻 sbi）
    NameTaken(&'static str),
    /// select/find/unregister 时名字不存在
    UnknownDevice(&'static str),
}

// ── SBI 设备：静态、无锁、panic 安全 ──────────────────────

/// SBI 直写 writer — 经 OpenSBI `console_putchar` ecall 逐字节输出。
///
/// 无锁、无 MMIO、无 hub 依赖。panic handler 与锁内调试（lock_debug!）专用。
/// 同时作为注册表中 sbi 设备的 writer 视图（同一实例，无双份）。
/// ZST——mprint!/mprintln! 复制实例调用（零开销），避免 static 可变借用。
#[derive(Clone, Copy)]
pub(crate) struct SbiWriter;

impl fmt::Write for SbiWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                crate::sbi::write_byte(b'\r');
            }
            crate::sbi::write_byte(b);
        }
        Ok(())
    }
}

/// 锁外无锁出口 — 静态常量，panic/锁内调试直写（不查表、不拿锁）。
pub(crate) static SBI_WRITER: SbiWriter = SbiWriter;

/// SBI 可变槽位 — 表空回落与表内 sbi 设备共享的句柄来源。
///
/// SbiWriter 是无状态 ZST，与 [`SBI_WRITER`] 为同一逻辑设备（输出行为相同）；
/// static mut 经 `addr_of!`/`addr_of_mut!` 访问（2021 edition，无 lint），
/// 唯一写者由调用方（print.rs 持 OUT_LOCK，关中断）保证。
static mut SBI_SLOT: SbiWriter = SbiWriter;

/// 表内 sbi 设备的共享视图（ensure_sbi 构造设备条目用）。
fn sbi_ref() -> &'static dyn fmt::Write {
    // SAFETY: SBI_SLOT 只经本模块访问；此处仅构造表内设备的共享句柄。
    unsafe { &*core::ptr::addr_of!(SBI_SLOT) }
}

/// 表空回落的可变句柄（preferred_writer 用）。
fn sbi_mut() -> &'static mut dyn fmt::Write {
    // SAFETY: 调用方（print.rs 持 OUT_LOCK）保证唯一写者；表空时无其他访问者。
    unsafe { &mut *core::ptr::addr_of_mut!(SBI_SLOT) }
}

/// sbi 常驻设备名
const SBI_NAME: &str = "sbi";

// ── 阶段状态机 ──────────────────────────────────────────

/// 输出阶段 — 显式区分 boot 早期（allocator 未就绪）与就绪。
///
/// Early：输出不得碰堆/注册表锁，静态直写 SBI；首个 register 后切 Ready。
/// 与 Linux earlycon → 正式 console 的分离对应。
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Phase {
    /// allocator 未就绪，输出只能静态直写 SBI
    Early = 0,
    /// 注册表已建立（至少含 sbi），输出走注册表
    Ready = 1,
}

static PHASE: AtomicU8 = AtomicU8::new(Phase::Early as u8);

// ── 注册表 ──────────────────────────────────────────────

/// 设备注册表 — 首项恒为 sbi（ensure_sbi 预置），其后按注册顺序排列。
static DEVICES: SpinLock<Vec<ConsoleDevice>> = SpinLock::new(Vec::new());

/// preferred 设备索引（PREFERRED_INVALID = 未固化 → 默认回落 sbi = 0）
const PREFERRED_INVALID: usize = usize::MAX;
static PREFERRED: AtomicUsize = AtomicUsize::new(PREFERRED_INVALID);

/// preferred 是否已固化（首个注册自动固化，或显式 select）——固化后
/// 后续 register 不再自动改默认目标（Linux preferred_console 语义）。
static PREFERRED_PINNED: AtomicBool = AtomicBool::new(false);

/// 确保注册表首项为 sbi 设备（首次访问时预置，幂等）。
///
/// 仅在写路径调用（register/select——Phase 1 allocator 就绪后）；
/// 查询路径（preferred_writer/preferred_buffer/find_writer）不得触发分配，
/// 表空直接回落 SBI / None。
fn ensure_sbi() {
    let mut list = DEVICES.lock();
    if list.is_empty() {
        list.push(ConsoleDevice::new(SBI_NAME, Some(sbi_ref()), None));
    }
}

// ── 公共 API ─────────────────────────────────────────────

/// 注册 console 设备 — 首个注册（sbi 之外）自动成为 preferred。
///
/// UART 驱动 probe 时经 `uart::register` 联动调用；名字冲突返回
/// [`DeviceError::NameTaken`]。
pub fn register(dev: ConsoleDevice) -> Result<(), DeviceError> {
    ensure_sbi();
    let mut list = DEVICES.lock();
    if list.iter().any(|d| d.name == dev.name) {
        return Err(DeviceError::NameTaken(dev.name));
    }
    let idx = list.len();
    list.push(dev);
    // 首个注册（无论是否成为 preferred）标志 allocator 已就绪 → 切 Ready；
    // 此后输出走注册表（Phase 1 的 uart 联动注册触发）。
    PHASE.store(Phase::Ready as u8, Ordering::Relaxed);
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
    if name == SBI_NAME {
        return Err(DeviceError::NameTaken(name));
    }
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(DeviceError::UnknownDevice(name))?;
    list.remove(idx);
    // preferred 索引修正：被移除位置之后整体前移
    let pref = PREFERRED.load(Ordering::Relaxed);
    if pref == idx {
        // 移除的是 preferred → 回落 sbi（0），允许后续 register 重新自动选择
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
    ensure_sbi();
    let list = DEVICES.lock();
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(DeviceError::UnknownDevice(name))?;
    PREFERRED.store(idx, Ordering::Relaxed);
    PREFERRED_PINNED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 当前 preferred 输出设备的可变句柄 — 调用方必须保证唯一写者。
///
/// 早期阶段（Phase::Early）：allocator 未就绪，静态直写 SBI——不碰注册表锁、
/// 不碰堆，boot 首个 println! 即可安全输出（对应 Linux earlycon）。
/// 就绪阶段（Phase::Ready）：取 preferred 设备（注册表恒非空，至少含 sbi）。
///
/// # Safety
///
/// 返回 'static `&mut`。唯一写者由调用方（print.rs 持 OUT_LOCK，关中断）
/// 保证；本模块不提供共享 `&` 视图，避免与写者别名。
pub(crate) fn preferred_writer() -> &'static mut dyn fmt::Write {
    if PHASE.load(Ordering::Relaxed) == Phase::Early as u8 {
        // SAFETY: 早期阶段单线程无并发写；sbi_mut 经 addr_of_mut! 访问。
        return sbi_mut();
    }
    // 输出链路内获取注册表锁（log_line → println! → write），见 print.rs 锁序
    let list = DEVICES.lock();
    // 防御：Ready 下表恒非空（sbi 常驻），此处防未来路径破坏不变量
    if list.is_empty() {
        return sbi_mut();
    }
    let idx = PREFERRED.load(Ordering::Relaxed).min(list.len() - 1);
    let dev = &list[idx];
    match dev.writer {
        // SAFETY: 调用方保证唯一写者（print.rs 持 OUT_LOCK）。
        Some(w) => unsafe { &mut *w.as_ptr() },
        // 防御：preferred 为仅输入设备（当前不存在，sbi 兜底）。
        None => sbi_mut(),
    }
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
pub(crate) fn find_writer(name: &'static str) -> Option<&'static mut dyn fmt::Write> {
    let list = DEVICES.lock();
    list.iter().find(|d| d.name == name).and_then(|dev| {
        // SAFETY: 调用方保证唯一写者（print.rs 持 OUT_LOCK）。
        dev.writer.map(|w| unsafe { &mut *w.as_ptr() })
    })
}

/// 已注册的 UART 设备数（不含常驻 sbi）— 启动日志与诊断用。
pub fn count() -> usize {
    let list = DEVICES.lock();
    list.iter().filter(|d| d.name != SBI_NAME).count()
}
