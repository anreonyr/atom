// CLINT (Core Local Interruptor) 驱动 — 定时器 + 软件中断
//
// ClintDriver 匹配 riscv,clint0 / sifive,clint0 / riscv,aclint-mtimer 设备；
// probe 构造 Clint 实例，挂载到设备 instance 并注册为内部中断控制器（hal）。
//
// S-mode 下 CLINT MMIO (0x02000000) 被 OpenSBI PMP 阻止，时间读数走 `time` CSR
// （Sstc 扩展），定时比较值经 SBI ecall 委托 M-mode。

use crate::driver::device::Device;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::InternalInterrupt;
use crate::memory::allocator::page;
use crate::sbi;

/// CLINT 控制器 — 提供内部中断（定时器 + IPI）能力。
#[derive(Debug)]
pub struct Clint {
    base: usize,
    timebase_freq: u64,
}

// SAFETY: single-hart kernel; MMIO base address is valid for the lifetime of the system.
unsafe impl Sync for Clint {}

impl Clint {
    pub const fn new(base: usize, timebase_freq: u64) -> Self {
        Clint {
            base,
            timebase_freq,
        }
    }
}

impl InternalInterrupt for Clint {
    fn frequency(&self) -> u64 {
        self.timebase_freq
    }

    fn read(&self) -> u64 {
        // S-mode 下 CLINT MMIO (0x02000000) 被 OpenSBI PMP 阻止。
        // 使用 `time` CSR（Sstc 扩展）直接读取时间，与 mtime 等价。
        let t: u64;
        unsafe { core::arch::asm!("csrr {}, time", out(reg) t) };
        t
    }

    fn next(&self, abs: u64) {
        // S-mode 下无法写 mtimecmp (M-only)，通过 SBI ecall 委托 OpenSBI
        sbi::set_timer(abs);
    }

    fn handle_timer(&self) {
        debug!("timer tick");
        self.next(self.read().wrapping_add(self.frequency()));
    }

    fn trigger_soft(&self, hart: u32) {
        // 每个 hart 的 MSIP 在 base + hart*4
        let msip = (self.base + hart as usize * 4) as *mut u32;
        unsafe {
            msip.write_volatile(1);
        }
    }
}

/// CLINT 驱动。
pub struct ClintDriver;

impl Driver for ClintDriver {
    fn name(&self) -> &'static str {
        "clint"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["riscv,clint0", "sifive,clint0", "riscv,aclint-mtimer"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // MMIO 映射（自含）
        unsafe { crate::memory::map_device(dev.base.as_usize(), dev.size, page::allocator()) }
            .map_err(|_| DriverError::MapFailed(dev.compatible))?;

        // 构造实例 + 挂载（Linux dev_set_drvdata 语义），时间频率取自 platform config
        let cfg = crate::platform::get();
        let clint = alloc::boxed::Box::leak(alloc::boxed::Box::new(Clint::new(
            dev.base.as_usize(),
            cfg.timebase_frequency,
        )));
        dev.set_instance(clint);

        // 装载首次定时中断（sie.STIE 由 init.rs Phase 3 统一使能）
        clint.next(clint.read() + clint.frequency());
        crate::hal::interrupt::register_internal(clint);

        Ok(())
    }
}

/// 驱动静态实例（controller::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &ClintDriver;
