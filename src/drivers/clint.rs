// CLINT (Core Local Interruptor) — RISC-V 平台级定时器
//
// QEMU virt 固定基地址 0x0200_0000
//   +0x4000  → MTIMECMP (hart 0)
//   +0xBFF8  → MTIME (64-bit 单调递增计数器)

use core::arch::asm;

use crate::hal::{IrqHandler, Timer};
use crate::trap;

/// CLINT 定时器控制器
pub struct Clint {
    base: usize,
}

// 单核 M-mode 下，MMIO 指针跨上下文（主循环 + 中断）安全
unsafe impl Sync for Clint {}

impl Clint {
    pub const fn new(base: usize) -> Self {
        Clint { base }
    }

    /// 一次性完成定时器中断配置：MTIE 使能 + 注册 + 首次触发
    pub fn init_timer(&'static self) {
        self.enable_irq();
        trap::register_timer(self);
        self.set_timer(<Self as Timer>::TICKS_PER_SEC);
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

impl IrqHandler for Clint {
    fn irq_number(&self) -> u32 {
        0 // 哨兵值——TIMER_HANDLER 注册入口不比较这个值
    }

    fn handle_irq(&self) {
        println!("[timer] tick!");
        self.set_timer(<Self as Timer>::TICKS_PER_SEC);
    }

    fn enable_irq(&self) {
        csr_set!(mie, 1 << 7); // mie.MTIE
    }
}

/// 全局 CLINT 实例（QEMU virt 默认基地址 0x0200_0000）
pub static CLINT: Clint = Clint::new(0x0200_0000);
