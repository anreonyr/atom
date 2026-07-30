// CLINT (Core Local Interruptor) — 定时器 + 软件中断
//
// 实现 InternalInterrupt trait：定时器（频率、时间、定时中断）+ 核间 IPI。
//
// MMIO 基址和定时器频率从 platform config 获取。
//   +0x0000  → MSIP (hart 0, 软件中断挂起位)
//   +0x4000  → MTIMECMP (hart 0, 定时器比较值)
//   +0xBFF8  → MTIME (64-bit 单调递增计数器, 只读)

use crate::hal::csr::sie::{self, Sie};
use crate::hal::{Driver, DriverError, InternalInterrupt};
use crate::sbi;

/// CLINT 控制器——提供内部中断（定时器 + IPI）能力
#[derive(Debug)]
pub struct Clint {
    base: usize,
    timebase_freq: u64,
}

// 单核 M-mode 下，MMIO 指针跨上下文（主循环 + 中断）安全
unsafe impl Sync for Clint {}

impl Clint {
    pub const fn new(base: usize, timebase_freq: u64) -> Self {
        Clint {
            base,
            timebase_freq,
        }
    }

    /// 使能监管者定时器中断 (sie.STIE)
    pub fn enable_timer_interrupt(&self) {
        unsafe { sie::set(Sie::STIE) };
    }

    /// 完整初始化定时器中断：使能 + 首次装载
    pub fn init_timer(&self) {
        self.enable_timer_interrupt();
        self.next(self.read() + self.frequency());
    }

    /// 使能监管者软件中断 (sie.SSIE)
    pub fn enable_soft_interrupt(&self) {
        unsafe { sie::set(Sie::SSIE) };
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

impl Driver for Clint {
    fn compatible() -> &'static str {
        "riscv,clint0"
    }

    fn init(&self) -> Result<(), DriverError> {
        self.enable_timer_interrupt();
        self.next(self.read() + self.frequency());
        Ok(())
    }
}

/// 创建 CLINT 实例（堆分配 + 'static 泄漏，引导期调用）。
///
/// 调用方自行通过 `device::register` 注册到全局注册中心。
pub(crate) fn init(base: usize, timebase_freq: u64) -> &'static Clint {
    let clint = alloc::boxed::Box::new(Clint::new(base, timebase_freq));
    alloc::boxed::Box::leak(clint)
}
