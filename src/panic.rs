// 内核 panic handler
//
// 改进点（相比之前只调用 error! 的实现）：
//   1. 直接通过 UART putc_raw 输出——绕过 SpinLock 避免死锁
//   2. 禁用中断——防止 panic 输出期间被中断干扰
//   3. 转储 CSR——scause（含解码）、sepc、sstatus、stval
//   4. RISC-V frame-pointer 回溯——从 s0 寄存器展开调用栈
//   5. 结构化输出——带 ANSI 颜色和分隔线，方便阅读
//   6. SBI system_reset 优雅关机（QEMU 退出，不再死循环）
//
// 输出示例：
//   ─── KERNEL PANIC ───────────────────────────────────────
//     panicked at src/main.rs:42:13:
//     assertion failed: x < 10
//
//   ─── CSRs ───────────────────────────────────────────────
//     scause:  0x0000000000000002  (Exception: Illegal instruction)
//     sepc:    0x0000000080201234
//     sstatus: 0x0000000000000120  (SPP=S, SPIE=1, SIE=0)
//     stval:   0x0000000000000000
//
//   ─── Backtrace ──────────────────────────────────────────
//     [0] 0x0000000080201234
//     [1] 0x0000000080205678
//
//   ─── System halted (shutting down via SBI) ──────────────

use core::fmt;
use core::panic::PanicInfo;

use crate::hal::csr::scause::{self, Scause};
use crate::hal::csr::sstatus::{self, Sstatus};
use crate::hal::csr::{sepc, stval};
use crate::lock::OnceLock;

/// 控制 panic 输出的详细程度。
///
/// 可通过 [`set_verbosity`] 在引导早期配置。未调用时默认 [`Full`](PanicVerbosity::Full)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PanicVerbosity {
    /// 仅输出 panic 消息和位置，随后关机。不输出 CSR 和回溯。
    Minimal,
    /// 输出 panic 消息 + CSR 寄存器转储。不输出回溯。
    Normal,
    /// 输出所有信息：消息、CSR、frame-pointer 回溯。
    Full,
}

static VERBOSITY: OnceLock<PanicVerbosity> = OnceLock::new();

/// 设置 panic 输出的详细程度。
///
/// 通常在 `init::run()` Phase 1 结束后调用一次。若从未调用，panic 时默认全量输出。
pub fn set_verbosity(level: PanicVerbosity) {
    let _ = VERBOSITY.set(level);
}

/// 返回当前配置的详细程度。未配置时返回 [`Full`](PanicVerbosity::Full)。
fn verbosity() -> PanicVerbosity {
    VERBOSITY.get().copied().unwrap_or(PanicVerbosity::Full)
}

/// panic 专用输出器——通过 SBI M-mode 写控制台，不经过任何 S-mode 锁。
///
/// 在 panic 上下文中：
/// - 中断已禁用，无并发问题
/// - 可能正处于持锁状态，必须绕过 SpinLock
/// - SBI ecall 是唯一不依赖 S-mode 驱动状态的输出方式
struct PanicWriter;

impl fmt::Write for PanicWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                crate::sbi::putchar(b'\r');
            }
            crate::sbi::putchar(b);
        }
        Ok(())
    }
}

/// panic 上下文中安全输出格式化字符串（绕过所有锁）
macro_rules! mprintln {
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

/// 返回 scause code 的人类可读描述
fn decode_scause(scause_val: Scause) -> &'static str {
    if scause_val.is_interrupt() {
        match scause_val.code() {
            1 => "Supervisor software interrupt",
            3 => "Machine software interrupt",
            5 => "Supervisor timer interrupt",
            7 => "Machine timer interrupt",
            9 => "Supervisor external interrupt",
            11 => "Machine external interrupt",
            _ => "Unknown interrupt",
        }
    } else {
        match scause_val.code() {
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

/// 用于回溯的初始栈顶偏移（DRAM_BASE + STACK_OFFSET）。
const STACK_OFFSET: usize = 8 * 1024 * 1024;
const MAX_BACKTRACE_FRAMES: usize = 16;

/// RISC-V frame-pointer 回溯
///
/// 从当前 s0 (x8) 寄存器开始，沿帧指针链向上遍历调用栈。
/// 每个栈帧布局（GCC/LLVM 标准序言）：
///   fp → [saved ra]     ← fp - 8
///        [saved fp]     ← fp - 16
///
/// 只返回 DRAM 范围内的合法地址。
fn backtrace() {
    let cfg = crate::platform::get();
    let dram_base = cfg.dram_base;
    let stack_top = dram_base + STACK_OFFSET;

    let mut fp: usize;
    // SAFETY: 读取 s0 寄存器（帧指针），无副作用
    unsafe {
        core::arch::asm!("mv {}, s0", out(reg) fp);
    }

    for i in 0..MAX_BACKTRACE_FRAMES {
        // 检查帧指针是否在合法内存范围内
        if !(dram_base..stack_top).contains(&fp) || fp < 16 {
            if i == 0 {
                mprintln!("    (no frame pointer available)");
            }
            break;
        }

        // SAFETY: 已验证 fp 在 DRAM 范围内
        let ra = unsafe { *((fp - 8) as *const usize) };
        let next_fp = unsafe { *((fp - 16) as *const usize) };

        mprintln!("    [{i}] {ra:#018x}");

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
        // 清零 sstatus.SIE，同时清除 sie 中的各中断使能位
        core::arch::asm!("csrc sstatus, {}", in(reg) 1usize << 1);
        core::arch::asm!("csrw sie, {}", in(reg) 0usize);
    }

    // 2. 读取所有 CSR（在 panic 发生时的快照）
    let scause_val = unsafe { scause::read() };
    let sepc_val = unsafe { sepc::read() };
    let sstatus_val = unsafe { sstatus::read() };
    let stval_val = unsafe { stval::read() };

    // 3. 输出结构化 panic 信息
    let red = "\x1b[31m";
    let bold = "\x1b[1m";
    let cyan = "\x1b[36m";
    let yellow = "\x1b[33m";
    let reset = "\x1b[0m";

    // ── 标题 ──
    mprintln!("");
    mprintln!("{red}{bold}─── KERNEL PANIC{reset}");

    // ── panic 消息 ──
    if let Some(location) = info.location() {
        mprintln!(
            "  {bold}panicked at {file}:{line}:{col}{reset}",
            file = location.file(),
            line = location.line(),
            col = location.column(),
        );
    }
    // PanicMessage 实现 Display trait，直接格式化即可
    let msg = info.message();
    mprintln!("  {bold}{msg}{reset}");
    mprintln!("");

    // ── CSR 转储 ──
    if verbosity() >= PanicVerbosity::Normal {
        let csr_label = format_args!("{cyan}─── CSRs{reset}");
        mprintln!("{csr_label}");

        let cause_type = if scause_val.contains(Scause::INTERRUPT) {
            "Interrupt"
        } else {
            "Exception"
        };
        mprintln!(
            "  scause:  {scause_val:#018x}  ({cause_type}: {detail})",
            detail = decode_scause(scause_val),
        );
        mprintln!("  sepc:    {sepc_val:#018x}");

        mprintln!(
            "  sstatus: {sstatus_val:#018x}  (SPP={spp}, SPIE={spie}, SIE={sie})",
            spp = if sstatus_val.bits() & crate::hal::csr::sstatus::SPP != 0 {
                'S'
            } else {
                'U'
            },
            spie = if sstatus_val.contains(Sstatus::SPIE) {
                1
            } else {
                0
            },
            sie = if sstatus_val.contains(Sstatus::SIE) {
                1
            } else {
                0
            },
        );
        mprintln!("  stval:   {stval_val:#018x}");
        mprintln!("");
    }

    // ── 回溯 ──
    if verbosity() >= PanicVerbosity::Full {
        mprintln!("{yellow}─── Backtrace{reset}");
        backtrace();
        mprintln!("");
    }

    // ── 结束 ──
    mprintln!("{red}{bold}─── System halted (shutting down via SBI){reset}");

    // 4. 通过 SBI 调用关机（QEMU 退出）
    crate::sbi::system_reset(crate::sbi::RESET_TYPE_SHUTDOWN, 0);
}
