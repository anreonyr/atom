// 内核 panic handler
//
// 改进点（相比之前只调用 error! 的实现）：
//   1. 直接通过 UART putc_raw 输出——绕过 SpinLock 避免死锁
//   2. 禁用中断——防止 panic 输出期间被中断干扰
//   3. 转储 CSR——mcause（含解码）、mepc、mstatus、mtval
//   4. RISC-V frame-pointer 回溯——从 s0 寄存器展开调用栈
//   5. 结构化输出——带 ANSI 颜色和分隔线，方便阅读
//
// 输出示例：
//   ─── KERNEL PANIC ───────────────────────────────────────
//     panicked at src/main.rs:42:13:
//     assertion failed: x < 10
//
//   ─── CSRs ───────────────────────────────────────────────
//     mcause:  0x0000000000000002  (Exception: Illegal instruction)
//     mepc:    0x0000000080001234
//     mstatus: 0x0000000000001880  (MPP=M, MPIE=1, MIE=0)
//     mtval:   0x0000000000000000
//
//   ─── Backtrace ──────────────────────────────────────────
//     [0] 0x0000000080001234
//     [1] 0x0000000080005678
//
//   ─── System halted ──────────────────────────────────────

use core::fmt;
use core::panic::PanicInfo;

use crate::drivers::UART;
use crate::hal::csr::mcause::{self, Mcause};
use crate::hal::csr::mstatus::{mpp, Mstatus};
use crate::hal::csr::{mepc, mstatus, mtval};


/// panic 专用输出器——直接写 UART THR，不经过任何锁。
///
/// 在 panic 上下文中：
/// - 中断已禁用，无并发问题
/// - 可能正处于持锁状态，必须绕过 SpinLock
struct PanicWriter;

impl fmt::Write for PanicWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                UART.putc_raw(b'\r');
            }
            UART.putc_raw(b);
        }
        Ok(())
    }
}

/// panic 上下文中安全输出格式化字符串（绕过所有锁）
macro_rules! panic_println {
    ($($arg:tt)*) => {{
        let _ = fmt::Write::write_fmt(
            &mut $crate::panic::PanicWriter,
            format_args!($($arg)*),
        );
        // 显式输出换行
        let _ = fmt::Write::write_fmt(
            &mut $crate::panic::PanicWriter,
            format_args!("\n"),
        );
    }};
}


/// 返回 mcause code 的人类可读描述
fn decode_mcause(mcause_val: Mcause) -> &'static str {
    if mcause_val.is_interrupt() {
        match mcause_val.code() {
            1 => "Supervisor software interrupt",
            3 => "Machine software interrupt",
            5 => "Supervisor timer interrupt",
            7 => "Machine timer interrupt",
            9 => "Supervisor external interrupt",
            11 => "Machine external interrupt",
            _ => "Unknown interrupt",
        }
    } else {
        match mcause_val.code() {
            0 => "Instruction address misaligned",
            1 => "Instruction access fault",
            2 => "Illegal instruction",
            3 => "Breakpoint",
            4 => "Load address misaligned",
            5 => "Load access fault",
            6 => "Store/AMO address misaligned",
            7 => "Store/AMO access fault",
            8 => "Environment call from U-mode",
            9 => "Environment call from S-mode",
            11 => "Environment call from M-mode",
            12 => "Instruction page fault",
            13 => "Load page fault",
            15 => "Store/AMO page fault",
            _ => "Unknown exception",
        }
    }
}


const DRAM_BASE: usize = 0x8000_0000;
const STACK_TOP: usize = 0x8080_0000;
const MAX_BACKTRACE_FRAMES: usize = 16;

/// RISC-V frame-pointer 回溯
///
/// 从当前 s0 (x8) 寄存器开始，沿帧指针链向上遍历调用栈。
/// 每个栈帧布局（GCC/LLVM 标准序言）：
///   fp → [saved ra]     ← fp - 8
///        [saved fp]     ← fp - 16
///
/// 只返回 [DRAM_BASE, STACK_TOP) 范围内的合法地址。
fn backtrace() {
    let mut fp: usize;
    // SAFETY: 读取 s0 寄存器（帧指针），无副作用
    unsafe {
        core::arch::asm!("mv {}, s0", out(reg) fp);
    }

    for i in 0..MAX_BACKTRACE_FRAMES {
        // 检查帧指针是否在合法内存范围内
        if !(DRAM_BASE..STACK_TOP).contains(&fp) || fp < 16 {
            if i == 0 {
                panic_println!("    (no frame pointer available)");
            }
            break;
        }

        // SAFETY: 已验证 fp 在 DRAM 范围内
        let ra = unsafe { *((fp - 8) as *const usize) };
        let next_fp = unsafe { *((fp - 16) as *const usize) };

        panic_println!("    [{i}] {ra:#018x}");

        // 终止条件：帧指针为 0 或未变化（防止无限循环）
        if next_fp == 0 || next_fp >= fp {
            break;
        }
        fp = next_fp;
    }
}


#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    // 1. 禁用所有中断——防止 panic 输出中被中断打断
    unsafe {
        // 清零 mstatus.MIE，同时清除 mie 中的各中断使能位
        core::arch::asm!("csrc mstatus, {}", in(reg) 1usize << 3);
        core::arch::asm!("csrw mie, {}", in(reg) 0usize);
    }

    // 2. 读取所有 CSR（在 panic 发生时的快照）
    let mcause_val = unsafe { mcause::read() };
    let mepc_val = unsafe { mepc::read() };
    let mstatus_val = unsafe { mstatus::read() };
    let mtval_val = unsafe { mtval::read() };

    // 3. 输出结构化 panic 信息
    let red = "\x1b[31m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let yellow = "\x1b[33m";
    let reset = "\x1b[0m";

    // ── 标题 ──
    panic_println!("");
    panic_println!("{red}{bold}─── KERNEL PANIC{reset}");

    // ── panic 消息 ──
    if let Some(location) = info.location() {
        panic_println!(
            "  {bold}panicked at {file}:{line}:{col}{reset}",
            file = location.file(),
            line = location.line(),
            col = location.column(),
        );
    }
    // PanicMessage 实现 Display trait，直接格式化即可
    let msg = info.message();
    panic_println!("  {bold}{msg}{reset}");
    panic_println!("");

    // ── CSR 转储 ──
    let csr_label = format_args!("{cyan}─── CSRs{reset}");
    panic_println!("{csr_label}");

    let cause_type = if mcause_val.contains(Mcause::INTERRUPT) {
        "Interrupt"
    } else {
        "Exception"
    };
    panic_println!(
        "  mcause:  {mcause_val:#018x}  ({cause_type}: {detail})",
        detail = decode_mcause(mcause_val),
    );
    panic_println!("  mepc:    {mepc_val:#018x}");

    panic_println!(
        "  mstatus: {mstatus_val:#018x}  (MPP={mpp}, MPIE={mpie}, MIE={mie})",
        mpp = match mstatus_val.bits() & mpp::MASK {
            mpp::M => 'M',
            mpp::S => 'S',
            _ => 'U',
        },
        mpie = if mstatus_val.contains(Mstatus::MPIE) {
            1
        } else {
            0
        },
        mie = if mstatus_val.contains(Mstatus::MIE) {
            1
        } else {
            0
        },
    );
    panic_println!("  mtval:   {mtval_val:#018x}");
    panic_println!("");

    // ── 回溯 ──
    panic_println!("{yellow}─── Backtrace{reset}");
    backtrace();
    panic_println!("");

    // ── 结束 ──
    panic_println!("{red}{bold}─── System halted{reset}");

    // 4. 死循环 + WFI（系统在此停止）
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
