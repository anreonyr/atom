#![no_std]
#![no_main]
#![feature(allocator_api)]
#![feature(ptr_cast_slice)]
extern crate alloc;
mod platform;
mod sbi;
mod scheduler;

#[macro_use]
mod macros;

#[macro_use]
mod print;

#[macro_use]
mod log;

mod context;
mod demos;
mod driver;
mod file;
mod filesystem;
mod hal;
mod init;
mod lock;
mod memory;
mod panic;
mod trap;
mod uart;

use core::arch::{asm, global_asm};

use crate::hal::csr::{
    sie::{self, Sie},
    sstatus::{self, Sstatus},
};

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
/// # Safety
pub unsafe extern "C" fn early(hartid: usize, ptr: usize) -> ! {
    platform::init(ptr);

    let cfg: &platform::Config = platform::get();
    // boot 栈顶留一页守护余量：`dram_base + dram_size` 是 DRAM 恒等映射的
    // 第一个未映射字节，sp 恰好压顶时 trap_vector 保存帧（sp-264..sp-8 的
    // sd 指令）会越过上界触发缺页。栈顶下移一页后，即使 trap 时 sp 在栈顶，
    // 帧保存也始终落在映射区内。
    let stack_top = (cfg.dram_base + cfg.dram_size - crate::memory::PAGE_SIZE) & !15;

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

#[no_mangle]
/// # Safety
pub unsafe extern "C" fn main(hartid: usize) -> ! {
    panic::set_verbosity(panic::PanicVerbosity::Full);

    log::set_clock_source(&log::CSR_CLOCK, platform::get().timebase_frequency);
    log::set_max_level(log::LogLevel::Info);
    log::set_module_rules(&[log::ModuleRule {
        prefix: "driver::controller::clint",
        level: log::LogLevel::Info,
    }]);

    info!(
        "log ready — level {:?} (clint timer capped at Info)",
        log::max_level()
    );

    init::run().expect("kernel boot failed");

    info!(
        "hart {} booted, DRAM: {:#x}..{:#x} ({} MiB)",
        hartid,
        platform::get().dram_base,
        platform::get().dram_base + platform::get().dram_size,
        platform::get().dram_size / (1024 * 1024),
    );

    demos::run();

    sie::set(Sie::SEIE);
    sie::set(Sie::STIE);
    sie::set(Sie::SSIE);

    // SAFETY: 单 hart，刚完成使能，读 CSR 无副作用。
    let sie_val = unsafe { sie::read() };
    let sstatus_val = unsafe { sstatus::read() };
    info!(
        "interrupts enabled — sie={:#x} (SEIE|STIE|SSIE), sstatus.SIE={}",
        sie_val.bits(),
        sstatus_val.contains(Sstatus::SIE) as u8,
    );

    sstatus::set(Sstatus::SIE);

    loop {
        unsafe { asm!("wfi") }
    }
}
