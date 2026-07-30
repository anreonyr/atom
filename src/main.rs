#![no_std]
#![no_main]
#![feature(allocator_api)]
extern crate alloc;

mod allocator;
mod platform;
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
    ".globl _early_stack_top",
    ".globl _start",
    "_start:",
    // a0 = hartid, a1 = DTB 物理地址 (RISC-V Linux boot protocol)
    // la 只修改 sp，a0/a1 原样传递给 main
    "    la   sp, _early_stack_top",
    "    j    early",
);

#[no_mangle]
pub extern "C" fn early(hartid: usize, dtb_ptr: usize) -> ! {
    unsafe {
        platform::init(dtb_ptr);

        let cfg = platform::config();
        let stack_top = cfg.dram_base + cfg.dram_size;

        asm!(
            "mv   sp, {sp}",
            "mv   a0, {hartid}",
            "jalr zero, 0({main})",
            sp = in(reg) stack_top,
            hartid = in(reg) hartid,
            main = in(reg) main,
            options(noreturn),
        );
    }
}

#[no_mangle]
pub extern "C" fn main(hartid: usize) -> ! {
    init::run();

    info!(
        "hart {} booted, DRAM: {:#x}..{:#x} ({} MiB)",
        hartid,
        platform::config().dram_base,
        platform::config().dram_base + platform::config().dram_size,
        platform::config().dram_size / (1024 * 1024),
    );

    // 创建两个测试任务，调度器会在定时器中断时切换
    scheduler::spawn(task_a);
    scheduler::spawn(task_b);

    info!("idle task running (wfi loop)");

    loop {
        unsafe { asm!("wfi") }
    }
}

fn task_a() {
    let mut count = 0u64;
    loop {
        count += 1;
        info!("[A] count={}", count);
        for _ in 0..2_000_000 {
            unsafe { asm!("nop") }
        }
    }
}

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
