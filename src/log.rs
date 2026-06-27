// 内核日志系统
//
// 提供五级日志宏：error! / warn! / info! / debug! / trace!
//
// 两层过滤：
//   1. 编译期：COMPILE_MAX_LEVEL 以上的调用被编译器完全移除（零开销）
//   2. 运行时：set_max_level() 动态调整，默认 Info
//
// 输出格式：
//   [sec.usec LEVEL  module] message                  — Error / Warn / Info
//   [sec.usec LEVEL  module file:line] message        — Debug / Trace
//
// 所有宏自动捕获 module_path!()、file!()、line!()。
//
// 死锁安全性：
//   当前中断不可嵌套（M-mode mstatus.MIE 在进入时自动清除），主循环仅为
//   `wfi` 不输出日志，因此中断上下文中的日志不会与 OUTPUT 锁竞争。

use crate::lock::SpinLock;


/// 日志级别（按严重程度递增）
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}


/// 编译期最高日志级别——高于此级别的调用在宏展开中被编译器移除。
///
/// 发布构建时可改为 `LogLevel::Info` 以完全消除 debug/trace 代码。
pub const COMPILE_MAX_LEVEL: LogLevel = LogLevel::Trace;


/// 运行时最高日志级别（默认 Info：Error + Warn + Info 可见）
static RUNTIME_LEVEL: SpinLock<LogLevel> = SpinLock::new(LogLevel::Info);

/// 设置运行时最高日志级别
pub fn set_max_level(level: LogLevel) {
    RUNTIME_LEVEL.lock(|l| *l = level);
}

/// 获取当前运行时最高日志级别
pub fn max_level() -> LogLevel {
    RUNTIME_LEVEL.lock(|l| *l)
}


/// 时间戳函数指针和频率——init 时注册
static MTIME_FN: SpinLock<Option<(fn() -> u64, u64)>> = SpinLock::new(None);

/// 注册时间戳源（应在 init 阶段调用一次）。
///
/// `f` 返回当前 mtime 值，`freq` 为定时器频率 (Hz)。
pub fn init_timestamp(f: fn() -> u64, freq: u64) {
    MTIME_FN.lock(|slot| *slot = Some((f, freq)));
}

/// 读取已注册的 (mtime, frequency)（未注册则返回 None）
fn read_mtime() -> Option<(u64, u64)> {
    MTIME_FN.lock(|slot| slot.map(|(f, freq)| (f(), freq)))
}


/// 核心日志输出（由宏调用，不应直接使用）
#[doc(hidden)]
pub fn _log(level: LogLevel, args: core::fmt::Arguments, module: &str, file: &str, line: u32) {
    // 运行时级别检查
    if (level as u8) > (max_level() as u8) {
        return;
    }

    let (color, label) = match level {
        LogLevel::Error => ("\x1b[31m", "ERROR"),
        LogLevel::Warn => ("\x1b[33m", "WARN "),
        LogLevel::Info => ("\x1b[32m", "INFO "),
        LogLevel::Debug => ("\x1b[36m", "DEBUG"),
        LogLevel::Trace => ("\x1b[35m", "TRACE"),
    };
    let reset = "\x1b[0m";

    // 时间戳内联格式化——避免额外依赖
    match read_mtime() {
        Some((mt, freq)) => {
            let sec = mt / freq;
            let usec = if freq >= 1_000_000 {
                (mt % freq) / (freq / 1_000_000)
            } else {
                (mt % freq) / 10
            };
            match level {
                LogLevel::Error | LogLevel::Warn | LogLevel::Info => {
                    println!(
                        "[{}.{:06} {}{} {}{}] {}",
                        sec, usec, color, label, module, reset, args,
                    );
                }
                LogLevel::Debug | LogLevel::Trace => {
                    println!(
                        "[{}.{:06} {}{} {} {}:{}{}] {}",
                        sec, usec, color, label, module, file, line, reset, args,
                    );
                }
            }
        }
        None => match level {
            LogLevel::Error | LogLevel::Warn | LogLevel::Info => {
                println!("[?.?????? {}{} {}{}] {}", color, label, module, reset, args,);
            }
            LogLevel::Debug | LogLevel::Trace => {
                println!(
                    "[?.?????? {}{} {} {}:{}{}] {}",
                    color, label, module, file, line, reset, args,
                );
            }
        },
    }
}


/// 条件日志（内部使用，通过具体级别宏调用）
///
/// 编译期常量比较使编译器在低级别构建中优化掉高于 COMPILE_MAX_LEVEL 的调用。
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        if ($level as u8) <= ($crate::log::COMPILE_MAX_LEVEL as u8) {
            $crate::log::_log(
                $level,
                format_args!($($arg)*),
                module_path!(),
                file!(),
                line!(),
            );
        }
    }};
}

/// 错误日志——最高严重级别
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Error, $($arg)*) };
}

/// 警告日志
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Warn, $($arg)*) };
}

/// 信息日志（默认运行时可见）
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Info, $($arg)*) };
}

/// 调试日志（默认运行时不可见，需 set_max_level(Debug) 启用）
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Debug, $($arg)*) };
}

/// 跟踪日志——最低级别，最详细
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Trace, $($arg)*) };
}
