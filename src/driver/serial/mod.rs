// 串口设备驱动目录 — 一种设备类型一个文件夹，一个型号一个文件
pub mod uart16550;

use crate::driver::traits::Driver;

/// 串口驱动的汇总（bus 聚合用）。
///
/// 同类型不同型号（如 16550 / PL011）各自一个 Driver，在此并列表出。
pub const DRIVERS: &[&dyn Driver] = &[uart16550::DRIVER];
