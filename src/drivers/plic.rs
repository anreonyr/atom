// PLIC (Platform-Level Interrupt Controller)
//
// 实现 ExternalInterrupt trait：管理平台级外部中断路由。
//
// MMIO 基址从 platform config 获取。
// QEMU virt 提供 2 个上下文: context 0 = M-mode, context 1 = S-mode
//
// 寄存器布局：
//   BASE + source * 4                            → 中断优先级
//   BASE + 0x001000 + word * 4                   → 挂起位
//   BASE + 0x002000 + context * 0x80 + word * 4  → 使能位
//   BASE + 0x200000 + context * 0x1000           → 优先级阈值
//   BASE + 0x200004 + context * 0x1000           → Claim / Complete
//
// S-mode 上下文 (context=1):
//   使能位基址: BASE + 0x2080
//   阈值: BASE + 0x201000
//   Claim/Complete: BASE + 0x201004

use crate::hal::{Driver, DriverError, ExternalInterrupt};
use crate::lock::OnceLock;

/// PLIC 中断控制器（S-mode 上下文）
#[derive(Debug)]
pub struct Plic {
    base: usize,
    context: usize,
}

unsafe impl Sync for Plic {}

impl Plic {
    pub const fn new(base: usize, context: usize) -> Self {
        Plic { base, context }
    }

    /// 设置中断源的优先级（PLIC 特有操作，不在 ExternalInterrupt trait 中）
    pub fn set_priority(&self, interrupt: u32, priority: u32) {
        let p = (self.base + interrupt as usize * 4) as *mut u32;
        unsafe { p.write_volatile(priority); }
    }
}

impl ExternalInterrupt for Plic {
    fn init(&self) {
        // 设置当前上下文的优先级阈值 = 0（接收所有优先级的中断）
        let thresh = (self.base + 0x200000 + self.context * 0x1000) as *mut u32;
        unsafe { thresh.write_volatile(0); }
    }

    fn enable(&self, interrupt: u32) {
        let word = (interrupt / 32) as usize;
        let bit = interrupt % 32;
        let addr =
            (self.base + 0x002000 + self.context * 0x80 + word * 4) as *mut u32;
        unsafe { addr.write_volatile(addr.read_volatile() | 1 << bit); }
    }

    fn claim(&self) -> u32 {
        let claim = (self.base + 0x200004 + self.context * 0x1000) as *const u32;
        unsafe { claim.read_volatile() }
    }

    fn complete(&self, interrupt: u32) {
        let comp = (self.base + 0x200004 + self.context * 0x1000) as *mut u32;
        unsafe { comp.write_volatile(interrupt) }
    }
}

impl Driver for Plic {
    fn compatible() -> &'static str {
        "riscv,plic0"
    }

    fn init(&self) -> Result<(), DriverError> {
        ExternalInterrupt::init(self);
        Ok(())
    }
}

static PLIC_INSTANCE: OnceLock<Plic> = OnceLock::new();

/// 初始化 PLIC 实例（引导早期调用一次）
pub(crate) fn init(base: usize, context: usize) {
    PLIC_INSTANCE.set(Plic::new(base, context)).expect("PLIC already initialized");
}

/// 获取 PLIC 实例引用
#[allow(non_snake_case)]
pub fn PLIC() -> &'static Plic {
    PLIC_INSTANCE.get().expect("PLIC not initialized")
}
