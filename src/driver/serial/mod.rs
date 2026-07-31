// 串口设备驱动目录 — 一种设备类型一个文件夹，一个型号一个文件
//
// 同时维护"已 probe 的 UART 注册表"：各型号驱动 probe 时把实例注册进来，
// 供 console 选择（serial::console）与 devfs 枚举（serial::all）。
// 设备发现（bus）仍按 compatible 匹配；注册表解决"跨型号取 UART"的问题。

pub mod sifive_uart;
pub mod uart16550;

use crate::driver::traits::Driver;
use crate::filesystem::traits::File;
use crate::lock::SpinLock;
use alloc::vec::Vec;

/// 串口驱动的汇总（bus 聚合用）。
///
/// 同类型不同型号（16550 / SiFive UART）各自一个 Driver，在此并列表出。
pub const DRIVERS: &[&dyn Driver] = &[uart16550::DRIVER, sifive_uart::DRIVER];

/// 已 probe 的 UART 设备 — 同一实例的 File（devfs）与 Write（console）双视图。
#[derive(Clone, Copy)]
pub struct SerialDevice {
    /// VFS 文件能力（devfs 节点挂载）
    pub file: &'static dyn File,
    /// 输出能力（console writer）
    pub writer: &'static dyn core::fmt::Write,
}

/// UART 注册表 — 按 probe（设备发现）顺序排列。
static UARTS: SpinLock<Vec<SerialDevice>> = SpinLock::new(Vec::new());

/// 注册一个 UART 实例（驱动 probe 内调用，Linux 注册 tty 设备的对应物）。
pub fn register(file: &'static dyn File, writer: &'static dyn core::fmt::Write) {
    UARTS.lock().push(SerialDevice { file, writer });
}

/// 所有已注册 UART（供 devfs 枚举；第一个为 console）。
pub fn all() -> Vec<SerialDevice> {
    UARTS.lock().clone()
}

/// 第一个 UART 的输出 writer（console 选择）。
pub fn console() -> Option<&'static dyn core::fmt::Write> {
    UARTS.lock().first().map(|s| s.writer)
}
