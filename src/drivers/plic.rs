// PLIC (Platform-Level Interrupt Controller)
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

use crate::hal::InterruptController;

/// PLIC 中断控制器（S-mode 上下文）
pub struct Plic {
    base: usize,
    context: usize,
}

unsafe impl Sync for Plic {}

impl Plic {
    /// 创建 PLIC 实例。
    ///
    /// `context` 为 RISC-V PLIC 上下文索引 (0=M-mode, 1=S-mode)。
    pub const fn new(base: usize, context: usize) -> Self {
        Plic { base, context }
    }
}

impl InterruptController for Plic {
    fn init(&self) {
        // 设置当前上下文的优先级阈值 = 0（接收所有优先级的中断）
        let thresh = (self.base + 0x200000 + self.context * 0x1000) as *mut u32;
        unsafe { thresh.write_volatile(0); }
    }

    fn enable(&self, irq: u32) {
        let word = (irq / 32) as usize;
        let bit = irq % 32;
        let addr =
            (self.base + 0x002000 + self.context * 0x80 + word * 4) as *mut u32;
        unsafe { addr.write_volatile(addr.read_volatile() | 1 << bit); }
    }

    fn set_priority(&self, irq: u32, priority: u32) {
        let p = (self.base + irq as usize * 4) as *mut u32;
        unsafe { p.write_volatile(priority); }
    }

    fn claim(&self) -> u32 {
        let claim = (self.base + 0x200004 + self.context * 0x1000) as *const u32;
        unsafe { claim.read_volatile() }
    }

    fn complete(&self, irq: u32) {
        let comp = (self.base + 0x200004 + self.context * 0x1000) as *mut u32;
        unsafe { comp.write_volatile(irq) }
    }
}

/// 全局 PLIC 实例 — S-mode 上下文 (context=1)，引导期间从 platform config 初始化
pub static mut PLIC: Plic = Plic::new(0, 1);
