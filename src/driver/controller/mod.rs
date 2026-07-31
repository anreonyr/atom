// 控制器设备驱动目录 — 中断/定时等系统级控制器
pub mod clint;
pub mod plic;

use crate::driver::traits::Driver;

/// 控制器驱动的汇总（bus 聚合用）。
pub const DRIVERS: &[&dyn Driver] = &[plic::DRIVER, clint::DRIVER];
