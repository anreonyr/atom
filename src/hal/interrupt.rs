/// 内部中断、外部中断、中断处理器抽象

use crate::lock::OnceLock;

// ── 内部中断（Timer + IPI）─────────────────────────────────

/// 内部中断抽象 — 定时器 + 核间软中断（per-hart，如 CLINT/ACLINT）
pub trait InternalInterrupt: Send + Sync {
    /// 定时器频率（Hz）
    fn frequency(&self) -> u64;
    /// 读取当前时间计数值
    fn read(&self) -> u64;
    /// 设置下次定时中断的绝对时间值
    fn next(&self, abs: u64);
    /// 定时器中断处理（默认行为：以 frequency 为间隔重装）
    fn handle_timer(&self) {
        self.next(self.read().wrapping_add(self.frequency()));
    }
    /// 向目标 hart 发送核间中断（IPI）
    #[allow(dead_code)] // 跨核 IPI 预留（单 hart 未用）
    fn trigger_soft(&self, hart: u32);
}

static INTERNAL: OnceLock<&'static dyn InternalInterrupt> = OnceLock::new();

/// 注册内部中断控制器实例（引导期调用一次）
pub fn register_internal(t: &'static dyn InternalInterrupt) {
    if INTERNAL.set(t).is_err() {
        crate::warn!("internal interrupt already registered (register_internal called more than once)");
    }
}

/// 获取当前注册的内部中断控制器。
///
/// 返回 `None` 如果尚未注册（应在引导期注册完成后调用）。
pub fn get_internal() -> Option<&'static dyn InternalInterrupt> {
    INTERNAL.get().copied()
}

// ── 外部中断控制器（Platform-Level）────────────────────────

/// 外部中断控制器抽象 — 管理平台级外部中断的路由和 claim/complete
///
/// RISC-V 平台上不同实现：PLIC (QEMU virt)、APLIC、AIA IMSIC。
pub trait ExternalInterrupt: Send + Sync {
    /// Initialize the interrupt controller hardware.
    ///
    /// # Errors
    ///
    /// Returns an error description if hardware initialization fails.
    fn init(&self) -> core::result::Result<(), &'static str>;
    fn enable(&self, interrupt: u32);
    fn claim(&self) -> u32;
    fn complete(&self, interrupt: u32);
}

static EXTERNAL: OnceLock<&'static dyn ExternalInterrupt> = OnceLock::new();

/// 注册外部中断控制器实例（引导期调用一次）
pub fn register_external(ic: &'static dyn ExternalInterrupt) {
    if EXTERNAL.set(ic).is_err() {
        crate::warn!("external interrupt already registered (register_external called more than once)");
    }
}

/// 获取当前注册的外部中断控制器。
///
/// 返回 `None` 如果尚未注册（应在引导期注册完成后调用）。
pub fn get_external() -> Option<&'static dyn ExternalInterrupt> {
    EXTERNAL.get().copied()
}

// ── 中断处理器（设备驱动）─────────────────────────────────

/// 中断处理器抽象 — 所有会产生中断的设备统一实现
pub trait InterruptHandler: Send + Sync {
    fn interrupt_number(&self) -> u32;
    fn handle_interrupt(&self);
    fn enable_interrupt(&self);
}
