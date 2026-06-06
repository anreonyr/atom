// CLINT (Core Local Interruptor) 定时器
//
// QEMU virt: CLINT_BASE = 0x0200_0000
//   MTIMECMP = 0x4000  — 比较值 (hart 0)
//   MTIME    = 0xBFF8  — 64 位单调递增计数器

use crate::uart;

const CLINT_BASE: usize = 0x0200_0000;
const MTIMECMP: usize = 0x4000;
const MTIME: usize = 0xBFF8;

/// 频率约 10 MHz（QEMU virt 默认），1 秒 ≈ 10_000_000 ticks
pub const TICKS_PER_SEC: u64 = 10_000_000;

/// 设置在 interval 个 tick 后触发定时器中断
pub fn set_timer(interval: u64) {
    let mtime = (CLINT_BASE + MTIME) as *const u64;
    let mtimecmp = (CLINT_BASE + MTIMECMP) as *mut u64;
    unsafe {
        mtimecmp.write_volatile(mtime.read_volatile() + interval);
    }
}

/// 定时器中断回调
pub fn handler() {
    uart::UART.puts("[timer] tick!\n");
}
