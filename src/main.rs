#![no_std]
#![no_main]
#![feature(allocator_api)]
#![feature(ptr_cast_slice)]
extern crate alloc;
mod platform;
// sbi 的 mprint!/mprintln!（M-mode 无锁直写）供 panic/lockdep 裸名使用
#[macro_use]
mod sbi;
mod schedule;

#[macro_use]
mod macros;

// file 模块提前声明且 macro_use：println!/mprintln! 宏（file::io::print）
// 供全 crate 裸名使用
#[macro_use]
mod file;
pub use file::io;

mod runtime;
pub use runtime::{context, panicking, trap};

#[macro_use]
mod log;

mod clock;
mod demos;
mod driver;
mod hal;
mod init;
mod lock;
mod memory;
mod shell;

use core::arch::{asm, global_asm};

use crate::{
    hal::csr::{
        sie::{self, Sie},
        sstatus::{self, Sstatus},
    },
    schedule::TaskBuilder,
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

#[unsafe(no_mangle)]
/// # Safety
pub unsafe extern "C" fn early(hartid: usize, ptr: usize) -> ! {
    unsafe {
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
}

#[unsafe(no_mangle)]
/// # Safety
pub unsafe extern "C" fn main(hartid: usize) -> ! {
    unsafe {
        panicking::set_verbosity(panicking::PanicVerbosity::Full);

        clock::init(&clock::CSR_CLOCK, platform::get().timebase_frequency);
        log::set_max_level(log::LogLevel::Info);
        // console 显示级别降到 Warn：boot 阶段 info 日志不再刷屏（静默启动），
        // 仅 warn/error 上 console；ring 仍记录全量（/dev/log 可查 boot 历史）。
        log::set_console_level(log::LogLevel::Info);
        log::set_module_rules(&[log::ModuleRule {
            prefix: "driver::controller::clint",
            level: log::LogLevel::Info,
        }]);

        info!("log ready — level {:?}", log::max_level());

        init::run().expect("kernel boot failed");

        info!(
            "hart {} booted, DRAM: {:#x}..{:#x} ({} MiB)",
            hartid,
            platform::get().dram_base,
            platform::get().dram_base + platform::get().dram_size,
            platform::get().dram_size / (1024 * 1024),
        );

        // shell 取代 demo 为默认交互：boot 后常驻命令循环（阻塞读 console）。
        // DEMO_* 编译期开关保留，shell 内 `bench` 命令按开关运行 demos::run()。
        TaskBuilder::new(schedule::Entry::Kernel(shell::run)).spawn();
        // demos::run();

        // 装载首次定时中断（tick::start：10ms 粒度）——必须在 sie 使能之前，
        // 否则首个 STI 会在 mtimecmp 未装载时悬空。
        clock::start();

        sie::set(Sie::SEIE);
        sie::set(Sie::STIE);
        sie::set(Sie::SSIE);

        // SAFETY: 单 hart，刚完成使能，读 CSR 无副作用。
        let sie_val = sie::read();
        let sstatus_val = sstatus::read();
        info!(
            "interrupts enabled — sie={:#x} (SEIE|STIE|SSIE), sstatus.SIE={}",
            sie_val.bits(),
            sstatus_val.contains(Sstatus::SIE) as u8,
        );

        sstatus::set(Sstatus::SIE);

        loop {
            asm!("wfi")
        }
    }
}
