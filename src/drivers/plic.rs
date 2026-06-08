// PLIC (Platform-Level Interrupt Controller)
//
// QEMU virt: PLIC_BASE = 0x0C00_0000
//
// 寄存器布局：
//   BASE + source * 4                   → 中断优先级
//   BASE + 0x001000 + word * 4          → 挂起位
//   BASE + 0x002000 + ctx * 0x80 + word * 4  → 使能位
//   BASE + 0x200000 + ctx * 0x1000           → 优先级阈值
//   BASE + 0x200004 + ctx * 0x1000           → Claim / Complete

use crate::hal::InterruptController;

/// PLIC 中断控制器
pub struct Plic {
    base: usize,
    hart_id: usize,
}

unsafe impl Sync for Plic {}

impl Plic {
    pub const fn new(base: usize, hart_id: usize) -> Self {
        Plic { base, hart_id }
    }
}

impl InterruptController for Plic {
    fn init(&self) {
        // 设置当前上下文的优先级阈值 = 0（接收所有优先级的中断）
        let thresh = (self.base + 0x200000 + self.hart_id * 0x1000) as *mut u32;
        unsafe { thresh.write_volatile(0); }
    }

    fn enable(&self, irq: u32) {
        let word = (irq / 32) as usize;
        let bit = irq % 32;
        let addr =
            (self.base + 0x002000 + self.hart_id * 0x80 + word * 4) as *mut u32;
        unsafe { addr.write_volatile(addr.read_volatile() | 1 << bit); }
    }

    fn set_priority(&self, irq: u32, priority: u32) {
        let p = (self.base + irq as usize * 4) as *mut u32;
        unsafe { p.write_volatile(priority); }
    }

    fn claim(&self) -> u32 {
        let claim = (self.base + 0x200004 + self.hart_id * 0x1000) as *const u32;
        unsafe { claim.read_volatile() }
    }

    fn complete(&self, irq: u32) {
        let comp = (self.base + 0x200004 + self.hart_id * 0x1000) as *mut u32;
        unsafe { comp.write_volatile(irq) }
    }
}

pub static PLIC: Plic = Plic::new(0x0C00_0000, 0);
