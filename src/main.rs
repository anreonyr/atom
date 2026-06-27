#![no_std]
#![no_main]
#![feature(allocator_api)]
extern crate alloc;

mod allocator;
mod sbi;
mod scheduler;

#[macro_use]
mod macros;

#[macro_use]
mod print;

#[macro_use]
mod log;

mod drivers;
mod hal;
mod init;
mod lock;
mod mmu;
mod panic;
mod trap;

use core::arch::{asm, global_asm};

global_asm!(
    ".section .text._start",
    ".globl _start",
    "_start:",
    "    la   sp, 0x80800000",
    "    j    main",
);

/// 测试任务 A：打印递增计数器
fn task_a() {
    let mut count = 0u64;
    loop {
        count += 1;
        info!("[A] count={}", count);
        // busy-wait 延迟，让输出可读
        for _ in 0..2_000_000 {
            unsafe { asm!("nop") }
        }
    }
}

/// 测试任务 B：打印递增计数器
fn task_b() {
    let mut count = 0u64;
    loop {
        count += 1;
        info!("[B] count={}", count);
        for _ in 0..2_000_000 {
            unsafe { asm!("nop") }
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    init::run();

    // 创建两个测试任务，调度器会在定时器中断时切换
    scheduler::spawn(task_a);
    scheduler::spawn(task_b);

    info!("idle task running (wfi loop)");

    loop {
        unsafe { asm!("wfi") }
    }
}
