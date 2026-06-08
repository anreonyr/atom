// CLINT (Core Local Interruptor) — 定时器 + 软件中断
//
// QEMU virt 固定基地址 0x0200_0000
//   +0x0000  → MSIP (hart 0, 软件中断挂起位)
//   +0x4000  → MTIMECMP (hart 0, 定时器比较值)
//   +0xBFF8  → MTIME (64-bit 单调递增计数器, 只读)
//
// CLINT 产生两类中断，在 trap.rs 中以独立路径分发：
//   mcause=3 (MSI)  → 软件中断 → CLINT.handle_soft_irq()
//   mcause=7 (MTI)  → 定时器中断 → CLINT.handle_timer_irq()

use core::arch::asm;

use crate::hal::Timer;

/// CLINT 控制器——提供定时器和软件中断两组能力
pub struct Clint {
    base: usize,
}

// 单核 M-mode 下，MMIO 指针跨上下文（主循环 + 中断）安全
unsafe impl Sync for Clint {}

impl Clint {
    pub const fn new(base: usize) -> Self {
        Clint { base }
    }

    // ── 定时器中断 ──────────────────────────────────────

    /// 使能机器定时器中断 (mie.MTIE)
    pub fn enable_timer_irq(&self) {
        csr_set!(mie, 1 << 7);
    }

    /// 定时器中断服务例程：打印 tick 并重新装载下一次触发
    pub fn handle_timer_irq(&self) {
        println!("[timer] tick!");
        self.set_timer(<Self as Timer>::TICKS_PER_SEC);
    }

    /// 完整初始化定时器中断：使能 + 首次装载
    pub fn init_timer(&self) {
        self.enable_timer_irq();
        self.set_timer(<Self as Timer>::TICKS_PER_SEC);
    }

    // ── 软件中断 ────────────────────────────────────────

    /// 触发软件中断（写 MSIP=1）
    pub fn trigger_soft_irq(&self) {
        let msip = self.base as *mut u32;
        unsafe { msip.write_volatile(1); }
    }

    /// 清除软件中断挂起位（写 MSIP=0）
    fn clear_soft_irq(&self) {
        let msip = self.base as *mut u32;
        unsafe { msip.write_volatile(0); }
    }

    /// 使能机器软件中断 (mie.MSIE)
    pub fn enable_soft_irq(&self) {
        csr_set!(mie, 1 << 3);
    }

    /// 软件中断服务例程：清除挂起位
    pub fn handle_soft_irq(&self) {
        println!("[soft] IPI received");
        self.clear_soft_irq();
    }
}

impl Timer for Clint {
    const TICKS_PER_SEC: u64 = 10_000_000; // QEMU virt 默认约 10 MHz

    fn set_timer(&self, interval: u64) {
        let mtime = (self.base + 0xBFF8) as *const u64;
        let mtimecmp = (self.base + 0x4000) as *mut u64;
        unsafe {
            mtimecmp.write_volatile(mtime.read_volatile() + interval);
        }
    }

    fn read_mtime(&self) -> u64 {
        let mtime = (self.base + 0xBFF8) as *const u64;
        unsafe { mtime.read_volatile() }
    }
}

/// 全局 CLINT 实例（QEMU virt 默认基地址 0x0200_0000）
pub static CLINT: Clint = Clint::new(0x0200_0000);
