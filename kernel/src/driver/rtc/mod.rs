// RTC 设备驱动目录 — 墙上时钟（实时时钟）
//
// 驱动实现只暴露 crate::hal::rtc::Realtime 能力（硬件操作），probe 时注册到
// crate::hal::rtc 注册表——注册表与查询 API 在硬件能力契约层（hal/rtc.rs），
// 本目录不维护任何注册表或辅助（与 driver/uart 同构：一个型号一个文件）。

pub mod goldfish_rtc;

use crate::driver::traits::Driver;

/// RTC 驱动的汇总（hub 聚合用）。
pub const DRIVERS: &[&dyn Driver] = &[goldfish_rtc::DRIVER];
