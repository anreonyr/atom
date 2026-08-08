// print!/println! — 全内核输出通道（带锁，委托 io::device 选择目标设备）
//
// 分层：设备层（sbi/uart）← device（统一设备表 + preferred 选择）← print（带锁通道）
//       ← 格式化层（log.rs / panic.rs / lock_debug!）
//
// print.rs 只做一件事：串行化输出到目标设备。
//   - write(args)          → device::preferred_writer()（preferred 设备）
//   - twrite(name, args) → device::find_writer(name)（显式路由；设备不存在静默丢弃）
//
// 无锁输出（panic / 锁内调试）不经本模块——直接走 device::SBI_WRITER 静态常量，
// 不查表、不拿锁，保证任意持锁状态崩溃仍能输出。
//
// 宏家族：
//   print! / println!             → preferred 设备
//   tprint! / tprintln!(dev, ..)  → 指定设备（调试/测试显式路由，Linux 直接
//                                     open /dev/ttyN 的对应物，不改 preferred）

use core::fmt;

use crate::lock::SpinLock;

/// 输出串行化锁 — 关中断，保证唯一写者；与 sink 注册表锁同一顺序获取
/// （OUT_LOCK → DEVICES），锁序一致。
static OUT: SpinLock<()> = SpinLock::new(());

/// 输出到 preferred 设备（`device::preferred_writer`），带锁串行化。
pub fn write(args: fmt::Arguments) {
    let _guard = OUT.lock();
    let w = super::device::preferred_writer();
    let _ = w.write_fmt(args);
}

/// 输出原始字节到 preferred 设备 — 逐字节，与 println! 共享 OUT 锁。
///
/// 字节语义：`bytes` 的全部字节原样写出（`\n` → `\r\n` 由设备层转换）。
/// `Stdout::write`（File 视图）经本函数逐字节直写，非 UTF-8 不再被丢弃。
pub fn write_bytes(bytes: &[u8]) {
    let _guard = OUT.lock();
    let w = super::device::preferred_writer();
    w.write_bytes(bytes);
}

/// 输出到指定设备（`device::find_writer(name)`），带锁串行化。
///
/// 设备不存在时静默丢弃（调试/测试路由，不应影响主输出路径）。
/// 预留：tprint!/tprintln! 的核心，当前无调用方。
#[allow(dead_code)]
pub fn twrite(name: &'static str, args: fmt::Arguments) {
    let _guard = OUT.lock();
    let Some(w) = super::device::find_writer(name) else {
        return;
    };
    let _ = w.write_fmt(args);
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
        $crate::io::print::write_to($dev, format_args!($($arg)*));
    }};
}

/// 格式化输出到指定设备，自动附加换行 — 设备不存在静默。
#[macro_export]
macro_rules! tprintln {
    ($dev:expr) => { $crate::io::print::write_to($dev, format_args!("\n")) };
    ($dev:expr, $($arg:tt)*) => {{
        $crate::io::print::write_to($dev, format_args!("{}\n", format_args!($($arg)*)));
    }};
}

#[macro_export]
macro_rules! mprint {
    ($($arg:tt)*) => {{
        let mut _w = $crate::io::device::SBI_WRITER; // 复制 ZST 实例（零开销）
        let _ = core::fmt::Write::write_fmt(&mut _w, format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! mprintln {
    () => { $crate::mprint!("\n") };
    ($($arg:tt)*) => {{
        $crate::mprint!("{}\n", format_args!($($arg)*));
    }};
}
