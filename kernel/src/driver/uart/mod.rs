// 串口设备驱动目录 — 一种设备类型一个文件夹，一个型号一个文件
//
// 驱动实现只暴露 crate::hal::byte_channel::ByteChannel 能力（硬件操作），
// probe 时注册到 crate::io::console 终端核心（console / devfs 枚举）——
// File/中断适配都在终端核心（file/io/console.rs），本目录不维护任何注册表或
// 输出辅助。

pub mod sifive_uart;
pub mod uart16550;

use crate::driver::traits::Driver;

/// 串口驱动的汇总（hub 聚合用）。
///
/// 同类型不同型号（16550 / SiFive UART）各自一个 Driver，在此并列表出。
pub const DRIVERS: &[&dyn Driver] = &[uart16550::DRIVER, sifive_uart::DRIVER];
