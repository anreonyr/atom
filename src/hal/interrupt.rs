/// 中断控制器抽象 — 管理外部中断的路由、使能和 claim/complete
///
/// RISC-V 平台上不同实现：PLIC (QEMU virt)、APLIC、AIA IMSIC。
pub trait InterruptController: Send + Sync {
    fn init(&self);
    fn enable(&self, interrupt: u32);
    fn claim(&self) -> u32;
    fn complete(&self, interrupt: u32);
}

/// 中断处理器抽象 — 所有会产生中断的设备统一实现此 trait
pub trait InterruptHandler: Send + Sync {
    fn interrupt_number(&self) -> u32;
    fn handle_interrupt(&self);
    fn enable_interrupt(&self);
}

use crate::lock::OnceLock;

static INTC: OnceLock<&'static dyn InterruptController> = OnceLock::new();

/// 注册中断控制器实例（引导期调用一次）
pub fn register(ic: &'static dyn InterruptController) {
    if INTC.set(ic).is_err() {
        panic!("interrupt controller already registered");
    }
}

/// 获取当前注册的中断控制器
pub fn get() -> &'static dyn InterruptController {
    match INTC.get() {
        Some(&ic) => ic,
        None => panic!("interrupt controller not registered"),
    }
}
