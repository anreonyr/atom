// 打印设备选择层 — 输出目标路由表（Linux console_list + preferred_console 的对应物）
//
// 本层只回答一个问题："输出打向哪个设备"。职责：
//   - 维护打印设备目录（动态 register/unregister）
//   - 维护 preferred（当前输出目标）：首个注册的设备自动成为 preferred，
//     显式 select() 后固化不再自动改；preferred 被移除时回落 sbi
//   - 按名查询（find_mut），供调试显式路由（tprintln!）
//
// 设备来源复用能力契约层：UART 驱动 probe 时经 uart::register 联动注册，
// sink 不重新发现设备。SBI 是常驻设备（name = "sbi"，不可注销）。
//
// 早期阶段（显式状态机）：boot 早期 allocator 未就绪，输出路径不得碰堆——
// 用 Phase 状态把早期与就绪显式区分：Early 阶段 current_mut 静态直写 SBI
// （不查表、不拿注册表锁），首个 register 切 Ready 后才走注册表。
// 对应 Linux：earlycon（boot 早期静态输出）与正式 console（注册机制）分离。
//
// panic 路径经静态 SBI_WRITER（mprint!/mprintln!）无锁直写（不查表、不拿锁），
// 保证任意持锁状态崩溃仍能输出，与 Phase 无关。
//
// 对应 Linux：`console_list`（链表）+ `preferred_console`（首选槽位）。
// 依赖方向：sink → fmt::Write（设备视图）与 sbi（ecall）；print → sink（查目标）。
//
// 句柄模型：设备表存 NonNull（可变句柄），对外提供 `&mut` 视图——唯一写者
// 由 print.rs 的 OUT_LOCK（关中断）保证；共享 `&` 视图（如未来 /dev/console
// 的打开目标）需另行约定与写者互斥，暂不提供。

use alloc::vec::Vec;
use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::lock::SpinLock;

/// 打印设备 — 注册表条目：身份 + 输出句柄。
pub struct SinkDevice {
    /// 设备身份（按名 select/find/unregister）
    pub name: &'static str,
    /// 输出句柄（可变；唯一写者由 print.rs 的 OUT_LOCK 保证）
    pub(crate) writer: NonNull<dyn fmt::Write>,
}

impl SinkDevice {
    /// 构造打印设备（内部转为非空可变句柄）。
    pub fn new(name: &'static str, writer: &'static dyn fmt::Write) -> Self {
        SinkDevice {
            name,
            // SAFETY: writer 指向 'static 实例（UART writer 泄漏或 SBI 静态），非空。
            writer: unsafe {
                NonNull::new_unchecked(writer as *const dyn fmt::Write as *mut dyn fmt::Write)
            },
        }
    }
}

/// 设备选择错误
///
/// NameTaken 的载荷当前无读取方（register 的调用方丢弃错误），保留以携带诊断信息。
#[allow(dead_code)]
#[derive(Debug)]
pub enum SinkError {
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

/// 表空回落的可变句柄（current_mut 用）。
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

/// 打印设备注册表 — 首项恒为 sbi（ensure_sbi 预置），其后按注册顺序排列。
static DEVICES: SpinLock<Vec<SinkDevice>> = SpinLock::new(Vec::new());

/// preferred 设备索引（PREFERRED_INVALID = 未固化 → 默认回落 sbi = 0）
const PREFERRED_INVALID: usize = usize::MAX;
static PREFERRED: AtomicUsize = AtomicUsize::new(PREFERRED_INVALID);

/// preferred 是否已固化（首个注册自动固化，或显式 select）——固化后
/// 后续 register 不再自动改默认目标（Linux preferred_console 语义）。
static PREFERRED_PINNED: AtomicBool = AtomicBool::new(false);

/// 确保注册表首项为 sbi 设备（首次访问时预置，幂等）。
///
/// 仅在写路径调用（register/select——Phase 1 allocator 就绪后）；
/// 查询路径（current_mut/find_mut）不得触发分配，表空直接回落 SBI。
fn ensure_sbi() {
    let mut list = DEVICES.lock();
    if list.is_empty() {
        list.push(SinkDevice::new(SBI_NAME, sbi_ref()));
    }
}

/// 注册打印设备 — 首个注册（sbi 之外）自动成为 preferred。
///
/// UART 驱动 probe 时经 `uart::register` 联动调用；名字冲突返回
/// [`SinkError::NameTaken`]。
pub fn register(dev: SinkDevice) -> Result<(), SinkError> {
    ensure_sbi();
    let mut list = DEVICES.lock();
    if list.iter().any(|d| d.name == dev.name) {
        return Err(SinkError::NameTaken(dev.name));
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
pub fn unregister(name: &'static str) -> Result<(), SinkError> {
    let mut list = DEVICES.lock();
    if name == SBI_NAME {
        return Err(SinkError::NameTaken(name));
    }
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(SinkError::UnknownDevice(name))?;
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
pub fn select(name: &'static str) -> Result<(), SinkError> {
    ensure_sbi();
    let list = DEVICES.lock();
    let idx = list
        .iter()
        .position(|d| d.name == name)
        .ok_or(SinkError::UnknownDevice(name))?;
    PREFERRED.store(idx, Ordering::Relaxed);
    PREFERRED_PINNED.store(true, Ordering::Relaxed);
    Ok(())
}

/// 当前输出设备的可变句柄 — 调用方必须保证唯一写者。
///
/// 早期阶段（Phase::Early）：allocator 未就绪，静态直写 SBI——不碰注册表锁、
/// 不碰堆，boot 首个 println! 即可安全输出（对应 Linux earlycon）。
/// 就绪阶段（Phase::Ready）：取 preferred 设备（注册表恒非空，至少含 sbi）。
///
/// # Safety
///
/// 返回 'static `&mut`。唯一写者由调用方（print.rs 持 OUT_LOCK，关中断）
/// 保证；本模块不提供共享 `&` 视图，避免与写者别名。
pub(crate) fn current_mut() -> &'static mut dyn fmt::Write {
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
    // SAFETY: 调用方保证唯一写者（print.rs 持 OUT_LOCK）。
    unsafe { &mut *dev.writer.as_ptr() }
}

/// 按名查找输出设备的可变句柄 — 不改变 preferred（调试显式路由用）。
///
/// 零分配纯查询：早期阶段表为空，返回 None（tprintln! 是 boot 后调试路由）。
///
/// # Safety
///
/// 同 [`current_mut`]：调用方必须保证唯一写者。
/// 预留：tprint!/tprintln! 的查找路径，当前无调用方。
// clippy::mut_from_ref：`name` 仅作查找键，返回的 &mut 来自受 OUT_LOCK
// 保护的设备表（NonNull 存储），不从不可变参数派生借用——语义非误用。
#[allow(dead_code, clippy::mut_from_ref)]
pub(crate) fn find_mut(name: &'static str) -> Option<&'static mut dyn fmt::Write> {
    let list = DEVICES.lock();
    list.iter().find(|d| d.name == name).map(|dev| {
        // SAFETY: 调用方保证唯一写者（print.rs 持 OUT_LOCK）。
        unsafe { &mut *dev.writer.as_ptr() }
    })
}
