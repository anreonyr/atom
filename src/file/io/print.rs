// print!/println! — 格式化输出通道（委托 console 决策输出目标）
//
// 分层：device ← console（输出决策 + 串行化）← print（格式化宏）← log / 各模块
//
// print.rs 只做一件事：把格式化参数转发给 console 输出决策层。
//   - write(args)        → console::write（preferred 设备或 boot 早期 sbi）
//   - write_bytes(bytes) → console::write_bytes（字节语义，非 UTF-8 原样直写）
//   - twrite(name, args) → console::write_to（显式设备路由，调试）
//
// 无锁输出（panic / lockdep）不经本模块——调用方直接 `sbi::mprintln!`
// （sbi 模块，M-mode 控制台直写，见 src/sbi/mod.rs）。

use core::fmt;

/// 格式化输出 — 委托 console 决策目标（preferred 设备或早期 sbi）。
pub fn write(args: fmt::Arguments) {
    super::console::write(args);
}

/// 原始字节输出 — 委托 console（字节语义，与 println! 共享输出路径）。
pub fn write_bytes(bytes: &[u8]) {
    super::console::write_bytes(bytes);
}

/// 输出到指定设备（`device::find_writer(name)`）— 设备不存在静默丢弃。
///
/// 预留：tprint!/tprintln! 的核心，当前无调用方。
#[allow(dead_code)]
pub fn twrite(name: &'static str, args: fmt::Arguments) {
    super::console::write_to(name, args);
}

/// 格式化输出，无换行 — 目标为 preferred 设备。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::io::print::write(format_args!($($arg)*));
    }};
}

/// 格式化输出，自动附加换行 — 目标为 preferred 设备。
#[macro_export]
macro_rules! println {
    () => { $crate::io::print::write(format_args!("\n")) };
    ($($arg:tt)*) => {{
        $crate::io::print::write(format_args!("{}\n", format_args!($($arg)*)));
    }};
}

/// 格式化输出到指定设备，无换行 — 设备不存在静默。
#[macro_export]
macro_rules! tprint {
    ($dev:expr, $($arg:tt)*) => {{
        $crate::io::print::twrite($dev, format_args!($($arg)*));
    }};
}

/// 格式化输出到指定设备，自动附加换行 — 设备不存在静默。
#[macro_export]
macro_rules! tprintln {
    ($dev:expr) => { $crate::io::print::twrite($dev, format_args!("\n")) };
    ($dev:expr, $($arg:tt)*) => {{
        $crate::io::print::twrite($dev, format_args!("{}\n", format_args!($($arg)*)));
    }};
}
