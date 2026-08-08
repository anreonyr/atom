// SBI (Supervisor Binary Interface) — ecall 调用 OpenSBI 服务
//
// 内核运行在 S-mode，无法直接操作 M-mode 硬件（如 mtimecmp）。
// 通过 ecall 指令触发陷阱进入 M-mode (OpenSBI)，由固件代为执行。
//
// SBI 调用约定 (RISC-V SBI spec v2.0):
//   a7  = Extension ID (EID)
//   a6  = Function ID (FID)
//   a0..a5 = 参数
//   返回: a0 = error, a1 = value
//
// 当前使用:
//   - TIME 扩展: set_timer — 设置定时器
//   - SRST 扩展: system_reset — 系统复位（关机）

use core::arch::asm;
use core::fmt;

// ── SBI Extension IDs (EID) ──────────────────────────────────────────

/// Legacy: 控制台字符输出（兼容所有 SBI 实现）
const EID_LEGACY_CONSOLE_PUTCHAR: usize = 0x01;
/// 定时器扩展
const EID_TIME: usize = 0x54494D45;
/// 系统复位扩展
const EID_SRST: usize = 0x53525354;

// ── FID (Function ID) ───────────────────────────────────────────────

/// TIME: 设置定时器（绝对时间值，非间隔）
const TIME_SET_TIMER: usize = 0;

/// SRST: 系统复位
const SRST_RESET: usize = 0;

// ── SRST 复位类型 ───────────────────────────────────────────────────

/// 关机
pub const RESET_TYPE_SHUTDOWN: u32 = 0;
/// 冷重启（预留，当前仅关机）
#[allow(dead_code)]
pub const RESET_TYPE_COLD_REBOOT: u32 = 1;
/// 热重启（预留，当前仅关机）
#[allow(dead_code)]
pub const RESET_TYPE_WARM_REBOOT: u32 = 2;

/// SBI ecall 底层调用。
///
/// 将参数写入 a0..a7 寄存器后执行 ecall 指令。
/// 返回 (error_code, return_value)。
#[inline(always)]
unsafe fn ecall(ext_id: usize, func_id: usize, args: [usize; 6]) -> (usize, usize) {
    unsafe {
        let error: usize;
        let value: usize;
        asm!(
            "ecall",
            inlateout("a0") args[0] => error,
            inlateout("a1") args[1] => value,
            in("a2") args[2],
            in("a3") args[3],
            in("a4") args[4],
            in("a5") args[5],
            in("a6") func_id,
            in("a7") ext_id,
        );
        (error, value)
    }
}

/// 通过 SBI legacy `console_putchar` 输出一个字符。
///
/// 委托 M-mode (OpenSBI) 写入控制台，绕过 S-mode 驱动。
/// 在 panic 上下文中安全使用——无需锁，无需 MMIO 映射。
#[inline(always)]
pub fn write_byte(ch: u8) {
    unsafe {
        asm!(
            "ecall",
            in("a0") ch as usize,
            in("a7") EID_LEGACY_CONSOLE_PUTCHAR,
            // legacy SBI 在 a0/a1 返回未指定值，标记为 clobber
            lateout("a0") _,
            lateout("a1") _,
        );
    }
}

/// 逐字节无锁输出到 M-mode 控制台（`\n` → `\r\n`）。
///
/// panic / 锁内调试专用：无锁、无需 MMIO 映射，任意持锁状态崩溃仍能输出。
/// `mprint!`/`mprintln!` 宏与 [`write_fmt`] 的底层。
#[inline]
pub fn write_bytes(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(b);
    }
}

/// 格式化无锁输出到 M-mode 控制台 — `mprint!`/`mprintln!` 宏的底层。
pub fn write_fmt(args: fmt::Arguments) {
    struct SbiConsole;
    impl fmt::Write for SbiConsole {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            write_bytes(s.as_bytes());
            Ok(())
        }
    }
    let _ = fmt::write(&mut SbiConsole, args);
}

/// 设置定时器，`stime_value` 为**绝对**时钟值。
///
/// `stime_value` = 当前 mtime + 间隔 ticks。
/// 当 mtime 达到 stime_value 时触发定时器中断。
///
/// SBI v2.0+ 约定: a0=低 32 位, a1=高 32 位。
#[inline(always)]
pub fn set_timer(stime_value: u64) {
    let lo = stime_value as u32 as usize;
    let hi = (stime_value >> 32) as u32 as usize;
    let _ = unsafe { ecall(EID_TIME, TIME_SET_TIMER, [lo, hi, 0, 0, 0, 0]) };
}

/// 系统复位（关机 / 重启）。
///
/// # Safety
///
/// 此函数永不返回（QEMU 退出或重启）。调用后代码不可达。
///
/// `reset_type`: [`RESET_TYPE_SHUTDOWN`], [`RESET_TYPE_COLD_REBOOT`], [`RESET_TYPE_WARM_REBOOT`] 之一。
/// `reset_reason`: 平台定义的原因码（QEMU 下通常为 0）。
#[inline(always)]
pub fn system_reset(reset_type: u32, reset_reason: u32) -> ! {
    let args = [reset_type as usize, reset_reason as usize, 0, 0, 0, 0];
    unsafe {
        ecall(EID_SRST, SRST_RESET, args);
    }
    // 如果 SBI 调用失败返回（不应发生），进入死循环
    loop {
        unsafe { asm!("wfi") };
    }
}

// ── 无锁输出宏（M-mode 控制台直写）─────────────────────────

/// 无锁格式化输出，无换行 — M-mode 控制台直写。
///
/// panic / lockdep 死锁报告专用（本模块 [`write_fmt`] 的宏封装）：
/// 不拿任何锁、无需 MMIO 映射，任意持锁状态崩溃仍能输出。
/// 正常输出走 `println!`（file::io::print → console），不经本宏。
#[macro_export]
macro_rules! mprint {
    ($($arg:tt)*) => {{
        $crate::sbi::write_fmt(format_args!($($arg)*));
    }};
}

/// 无锁格式化输出，自动附加换行 — M-mode 控制台直写。
#[macro_export]
macro_rules! mprintln {
    () => { $crate::mprint!("\n") };
    ($($arg:tt)*) => {{
        $crate::mprint!("{}\n", format_args!($($arg)*));
    }};
}
