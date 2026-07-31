// 设备驱动子系统 — bus/device/driver 模型
//
// 模块结构：
//   traits.rs   — Driver trait + DriverError（数据式驱动抽象）
//   device.rs   — Device + DeviceState + DTB 设备发现
//   bus.rs      — Bus：设备发现 + 匹配 + probe 编排 + 实例查询
//   serial/     — 串口设备驱动（uart16550，按型号分文件）
//   controller/ — 控制器设备驱动（plic, clint）

pub mod bus;
pub mod controller;
pub mod device;
pub mod serial;
pub mod traits;

// Driver 为对外 API（驱动模块内部经 traits::Driver 引用），此处 re-export 供外部使用。
#[allow(unused_imports)]
pub use traits::{Driver, DriverError};

/// 设备驱动子系统初始化：总线发现设备 → 匹配驱动 → deferred probe。
pub(crate) fn init() -> Result<(), DriverError> {
    bus::init()
}
