#![no_std]
#![no_main]

#[macro_use]
mod macros;
mod timer;
mod trap;
mod uart;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

global_asm!(
    ".section .text._start",
    ".globl _start",
    "_start:",
    "    la   sp, 0x80800000",
    "    j    rust_main",
);

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    // 设置陷阱向量
    csr_write!(mtvec, trap::trap_vector as *const () as usize);

    // 设置定时器
    timer::set_timer(timer::TICKS_PER_SEC);

    // 开启中断
    csr_set!(mie, 1 << 7); // mie.MTIE
    csr_set!(mstatus, 1 << 3); // mstatus.MIE

    uart::puts("timer interrupts enabled\n");

    loop {
        unsafe { asm!("wfi") }
    }
}

#[panic_handler]
fn panic_handler(_info: &PanicInfo) -> ! {
    uart::puts("[panic]\n");
    loop {
        unsafe { asm!("wfi") }
    }
}
