// print!/println! — 按特权级别分层的输出原语
//
// M-mode (SBI ecall)     — mprint!/mprintln!   无锁，始终可用，早期引导/panic 安全
// S-mode (UART)          — print!/println!      S_WRITER boot 时指向 MWriter（SBI），
//                                               print::init() 后切到 UART
// U-mode (ecall/syscall) — 将来用户态 ecall 陷入内核，走 VFS /dev/console 代理输出
//
// log 模块 (error!/warn!/info!/debug!/trace!) 是 S-mode print 的消费者：
//   _log() 格式化时间戳/级别/模块/颜色，然后调用 println!() 输出。
//   日志和 print 共享同一 S-mode 通道——同一个 UART 串行化输出。
//
// writer 从 boot 起始终有效（早期为 MWriter），因此输出路径无分支：
// 早期日志经 M-mode SBI 实时可见，console 就绪后自动切换 UART，格式完全一致。
//
// M-mode 输出委托 OpenSBI 通过 ecall 写控制台，绕过整个 S-mode 驱动栈。
// S-mode 输出经 UART (fmt::Write) → MMIO。
// U-mode 输出将在用户态实现后通过 syscall → VFS 路径到达 /dev/console。

use core::ptr::NonNull;

use crate::lock::SpinLock;

// ═══════════════════════════════════════════════════════════════════
// M-mode — SBI ecall，无锁，始终可用
// ═══════════════════════════════════════════════════════════════════

/// M-mode writer：通过 SBI `console_putchar` ecall 逐字节输出。
///
/// 无锁、无 MMIO 依赖、无 hub 依赖。panic handler 中安全使用。
/// ZST——每次在宏中构造实例，零运行时开销。
pub(crate) struct MWriter;

impl core::fmt::Write for MWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                crate::sbi::putchar(b'\r');
            }
            crate::sbi::putchar(b);
        }
        Ok(())
    }
}

/// M-mode writer 静态实例——S_WRITER 的 boot 初始值。
static MWRITER: MWriter = MWriter;

/// M-mode 格式化输出，无换行。
///
/// 每次构造 MWriter 实例（ZST，零运行时开销）。
#[macro_export]
macro_rules! mprint {
    ($($arg:tt)*) => {{
        let mut _w = $crate::print::MWriter;
        let _ = core::fmt::Write::write_fmt(&mut _w, format_args!($($arg)*));
    }};
}

/// M-mode 格式化输出，自动附加换行。
#[macro_export]
macro_rules! mprintln {
    () => { $crate::mprint!("\n") };
    ($($arg:tt)*) => {
        $crate::mprint!("{}\n", format_args!($($arg)*));
    };
}

// ═══════════════════════════════════════════════════════════════════
// S-mode — 全局 writer（boot 早期为 MWriter，init 后为 UART）
// ═══════════════════════════════════════════════════════════════════

/// 全局输出 writer：非空指针，指向 `dyn fmt::Write` 胖指针。
///
/// boot 早期指向 MWriter（SBI ecall），`print::init()` 后替换为 UART 实例。
pub(crate) struct SWriter {
    pub(crate) ptr: NonNull<dyn core::fmt::Write>,
}

// SAFETY: writer 指向 'static 实例（MWriter / UART），均 Sync；
// S_WRITER 锁（关中断）串行化所有写操作。
unsafe impl Send for SWriter {}
unsafe impl Sync for SWriter {}

/// 全局输出 writer 存储——锁同时负责串行化写操作与 writer 替换。
///
/// 从 boot 起始终有效：初始为 MWriter，保证 console 就绪前的早期日志实时可见；
/// `print::init()` 时替换为 UART 实例。writer 替换仅发生在引导期，无并发竞争。
pub(crate) static S_WRITER: SpinLock<SWriter> = SpinLock::new(SWriter {
    // SAFETY: MWRITER 是 'static 实例，指针非空。
    ptr: unsafe {
        NonNull::new_unchecked(
            &MWRITER as &dyn core::fmt::Write as *const dyn core::fmt::Write as *mut dyn core::fmt::Write
        )
    },
});

/// 初始化 S-mode 输出——将 writer 从 MWriter 替换为 UART 实例。
///
/// log 模块和所有 `print!`/`println!` 调用都依赖此初始化。
pub fn init() {
    // console 选择：serial 目录注册表里的第一个 UART（跨型号）
    let Some(writer) = crate::driver::serial::console() else {
        crate::mprintln!("print: no uart probed, keep SBI output");
        return;
    };
    *S_WRITER.lock() = SWriter {
        ptr: NonNull::from(writer),
    };
}

/// S-mode 格式化输出，无换行。
///
/// writer 从 boot 起始终有效（早期 MWriter / init 后 UART），输出路径无分支。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        let guard = $crate::print::S_WRITER.lock();
        // SAFETY: S_WRITER 锁（关中断）保证唯一 &mut；writer 指向 'static 实例。
        let _ = core::fmt::Write::write_fmt(
            unsafe { &mut *guard.ptr.as_ptr() },
            format_args!($($arg)*),
        );
    }};
}

/// S-mode 格式化输出，自动附加换行。
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*));
    };
}
