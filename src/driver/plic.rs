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

use super::Driver;
use crate::hal::ExternalInterrupt;

/// PLIC 中断控制器（S-mode 上下文）
#[derive(Debug)]
pub struct Plic {
    base: usize,
    context: usize,
}

// SAFETY: single-hart kernel; MMIO base address is valid for the lifetime of the system.
unsafe impl Sync for Plic {}

impl Plic {
    pub const fn new(base: usize, context: usize) -> Self {
        Plic { base, context }
    }

    /// 设置中断源的优先级（PLIC 特有操作，不在 ExternalInterrupt trait 中）
    pub fn set_priority(&self, interrupt: u32, priority: u32) {
        let p = (self.base + interrupt as usize * 4) as *mut u32;
        unsafe {
            p.write_volatile(priority);
        }
    }
}

impl ExternalInterrupt for Plic {
    fn init(&self) -> Result<(), &'static str> {
        // Set priority threshold = 0 (accept all priorities)
        let thresh = (self.base + 0x200000 + self.context * 0x1000) as *mut u32;
        unsafe {
            thresh.write_volatile(0);
        }
        Ok(())
    }

    fn enable(&self, interrupt: u32) {
        let word = (interrupt / 32) as usize;
        let bit = interrupt % 32;
        let addr = (self.base + 0x002000 + self.context * 0x80 + word * 4) as *mut u32;
        unsafe {
            addr.write_volatile(addr.read_volatile() | 1 << bit);
        }
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
    fn init(&'static self) -> Result<(), super::DriverError> {
        ExternalInterrupt::init(self).map_err(super::DriverError::Init)?;
        Ok(())
    }
}

/// Create and register a PLIC instance.
pub(crate) fn register(name: &'static str, base: usize, context: usize) -> &'static Plic {
    let plic = alloc::boxed::Box::new(Plic::new(base, context));
    let plic_ref = alloc::boxed::Box::leak(plic);
    super::hub::register::<Plic>(plic_ref, name);
    plic_ref
}
