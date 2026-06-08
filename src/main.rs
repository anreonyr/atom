#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod macros;
mod allocator;
mod plic;
mod timer;
mod trap;
mod uart;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

use uart::Uart;

global_asm!(
    ".section .text._start",
    ".globl _start",
    "_start:",
    "    la   sp, 0x80800000",
    "    j    rust_main",
);

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    // 初始化内核堆分配器（必须在任何 alloc 使用之前）
    allocator::init();

    // 设置陷阱向量
    csr_write!(mtvec, trap::trap_vector as *const () as usize);

    let uart = Uart::new(uart::UART_BASE);
    uart.init();
    uart.puts("atom kernel booted\r\n");

    // 初始化 PLIC
    plic::init();

    // 开启 UART 接收中断
    uart.enable_rx_interrupt();

    // 设置定时器
    timer::set_timer(timer::TICKS_PER_SEC);

    // 开启中断：MTIE（定时器）+ MEIE（外部）
    csr_set!(mie, (1 << 7) | (1 << 11)); // mie.MTIE | mie.MEIE
    csr_set!(mstatus, 1 << 3); // mstatus.MIE

    uart.puts("[info] interrupts enabled (MTI + MEI)\r\n");

    loop {
        unsafe { asm!("wfi") }
    }
}

#[panic_handler]
fn panic_handler(_info: &PanicInfo) -> ! {
    uart::UART.puts("[panic]\n");
    loop {
        unsafe { asm!("wfi") }
    }
}
