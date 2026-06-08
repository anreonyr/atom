#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod macros;
mod allocator;

#[macro_use]
mod print;

#[macro_use]
mod log;

mod drivers;
mod hal;
mod init;
mod lock;
mod trap;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

global_asm!(
    ".section .text._start",
    ".globl _start",
    "_start:",
    "    la   sp, 0x80800000",
    "    j    main",
);

#[no_mangle]
pub extern "C" fn main() -> ! {
    init::run();

    loop {
        unsafe { asm!("wfi") }
    }
}

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    error!("{}", info);
    loop {
        unsafe { asm!("wfi") }
    }
}
