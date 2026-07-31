// PLIC (Platform-Level Interrupt Controller) 驱动 — controller/ 角色目录
//
// PlicDriver 匹配 riscv,plic0 / sifive,plic-1.0.0 设备；probe 构造 Plic
// 实例，挂载到设备 instance 并注册为外部中断控制器（hal）。
//
// QEMU virt 提供 2 个上下文: context 0 = M-mode, context 1 = S-mode
// 寄存器布局见 CLAUDE.md（PLIC registers 一节）。

use crate::driver::device::Device;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::ExternalInterrupt;
use crate::memory::allocator::page;

/// PLIC 中断控制器（S-mode 上下文）。
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

    /// 设置中断源的优先级（PLIC 特有操作，不在 ExternalInterrupt trait 中）。
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

/// PLIC 驱动。
pub struct PlicDriver;

impl Driver for PlicDriver {
    fn name(&self) -> &'static str {
        "plic"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["riscv,plic0", "sifive,plic-1.0.0"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // MMIO 映射（自含）
        unsafe { crate::memory::map_device(dev.base.as_usize(), dev.size, page::allocator()) }
            .map_err(|_| DriverError::MapFailed(dev.compatible))?;

        // 构造实例 + 挂载（Linux dev_set_drvdata 语义）
        let plic = alloc::boxed::Box::leak(alloc::boxed::Box::new(Plic::new(
            dev.base.as_usize(),
            1, // S-mode context
        )));
        dev.set_instance(plic);

        // 硬件初始化（阈值）+ 注册外部中断控制器
        plic.init().map_err(DriverError::Init)?;
        crate::hal::interrupt::register_external(plic);

        Ok(())
    }
}

/// 驱动静态实例（controller::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &PlicDriver;
