// print!/println! — 按特权级别分层的输出原语
//
// M-mode (SBI ecall)     — mprint!/mprintln!   无锁，始终可用，早期引导/panic 安全
// S-mode (ConsoleDev)    — print!/println!      经 S_LOCK → S_WRITER → ConsoleDev → UART
// U-mode (ecall/syscall) — 将来用户态 ecall 陷入内核，走 VFS /dev/console 代理输出
//
// log 模块 (error!/warn!/info!/debug!/trace!) 是 S-mode print 的消费者：
//   _log() 格式化时间戳/级别/模块/颜色，然后调用 println!() 输出。
//   日志和 print 共享同一 S-mode 通道——同一个 ConsoleDev 串行化输出。
//
// M-mode 输出委托 OpenSBI 通过 ecall 写控制台，绕过整个 S-mode 驱动栈。
// S-mode 输出经 ConsoleDev (fmt::Write) → hub → UART MMIO。
// U-mode 输出将在用户态实现后通过 syscall → VFS 路径到达 /dev/console。

use core::ptr::NonNull;

use crate::lock::{OnceLock, SpinLock};

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
// S-mode — ConsoleDev → hub → UART MMIO
// ═══════════════════════════════════════════════════════════════════

/// S-mode writer：非空指针，`init()` 写入一次，之后只读。
///
/// 指向 `ConsoleDev` 的 `dyn fmt::Write` 胖指针。
pub(crate) struct SWriter {
    pub(crate) ptr: NonNull<dyn core::fmt::Write>,
}

// SAFETY: ptr 在 init() 写入一次后不再变动。ConsoleDev 是 Sync ZST，
// 实际可变状态在 hub 中，S_LOCK 串行化所有写操作。
unsafe impl Send for SWriter {}
unsafe impl Sync for SWriter {}

/// S-mode writer 存储——运行时 `init()` 注册一次，之后只读。
pub(crate) static S_WRITER: OnceLock<SWriter> = OnceLock::new();

/// S-mode 输出串行化锁。
pub(crate) static S_LOCK: SpinLock<()> = SpinLock::new(());

/// 初始化 S-mode 输出——将 ConsoleDev 注册为 writer。
///
/// log 模块和所有 `print!`/`println!` 调用都依赖此初始化。
pub fn init() {
    S_WRITER
        .set(SWriter {
            ptr: NonNull::from(&crate::filesystem::dev::console::CONSOLE),
        })
        .ok();
}

/// S-mode 格式化输出，无换行。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        let _guard = $crate::print::S_LOCK.lock();
        if let Some(sw) = $crate::print::S_WRITER.get() {
            // SAFETY: S_LOCK 保证同一时刻只有一个 &mut。
            let _ = core::fmt::Write::write_fmt(
                unsafe { &mut *sw.ptr.as_ptr() },
                format_args!($($arg)*),
            );
        }
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
