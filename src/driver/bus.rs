// 设备总线 — 设备发现、驱动匹配、probe 编排、设备实例查询
//
// 一个组件承担 Linux `bus_type` + 子系统注册表的职责：
//   引导期：`device::probe()` 发现设备 → 按 compatible 匹配驱动 → deferred probe
//   运行期：`bus::find::<T>()` 按具体类型查询设备实例（instance downcast）

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::driver::device::{self, Device, DeviceState};
use crate::driver::traits::{Driver, DriverError};
use crate::lock::{OnceLock, RwLock};

/// 设备总线。
pub struct Bus {
    /// 发现到的设备（引导期填充，运行期只读）
    devices: RwLock<Vec<Device>>,
    /// 驱动表（各角色目录 `DRIVERS` 的聚合，静态）
    drivers: &'static [&'static dyn Driver],
}

static BUS: OnceLock<&'static Bus> = OnceLock::new();

/// 初始化设备总线：发现设备 → 匹配驱动 → deferred probe。
///
/// 必须先 `BUS.set` 再 `probe_all`——驱动 probe 内会调 `bus::find`。
pub fn init() -> Result<(), DriverError> {
    let bus = Box::leak(Box::new(Bus {
        devices: RwLock::new(device::probe()),
        drivers: drivers(),
    }));
    if BUS.set(bus).is_err() {
        panic!("bus already initialized");
    }
    bus.probe()
}

/// 聚合各角色目录（serial/controller）的驱动表。
fn drivers() -> &'static [&'static dyn Driver] {
    static DRIVERS: OnceLock<Vec<&'static dyn Driver>> = OnceLock::new();
    DRIVERS.get_or_init(|| {
        super::serial::DRIVERS
            .iter()
            .chain(super::controller::DRIVERS.iter())
            .copied()
            .collect()
    })
}

impl Bus {
    /// deferred probe 编排循环。
    ///
    /// 每轮先收集待处理设备指针（释放 RwLock 后再调用 probe，避免重入），
    /// 逐设备匹配驱动并 probe；一轮内无进展则说明存在依赖环。
    fn probe(&self) -> Result<(), DriverError> {
        loop {
            let pending: Vec<*const Device> = self
                .devices
                .read()
                .iter()
                .filter(|d| !matches!(d.state(), DeviceState::Bound | DeviceState::Unsupported))
                .map(|d| d as *const Device)
                .collect();

            let mut progress = false;
            for ptr in pending {
                // SAFETY: probe_all 期间 devices 不扩容不移动，元素地址稳定；
                // RwLock guard 释放后 Vec 仍存储在 Bus 内，引用保持有效。
                let dev = unsafe { &*ptr };
                if matches!(dev.state(), DeviceState::Bound | DeviceState::Unsupported) {
                    continue;
                }
                let Some(drv) = self.match_driver(dev) else {
                    dev.set_state(DeviceState::Unsupported);
                    continue;
                };
                match drv.probe(dev) {
                    Ok(()) => {
                        dev.set_driver(drv);
                        dev.set_state(DeviceState::Bound);
                        progress = true;
                    }
                    Err(DriverError::Deferred) => dev.set_state(DeviceState::Deferred),
                    Err(e) => return Err(e),
                }
            }

            let all_done = self
                .devices
                .read()
                .iter()
                .all(|d| matches!(d.state(), DeviceState::Bound | DeviceState::Unsupported));
            if all_done {
                break;
            }
            if !progress {
                return Err(DriverError::Stuck);
            }
        }
        Ok(())
    }

    /// 按 compatible 匹配驱动（id_table 语义）。
    fn match_driver(&self, dev: &Device) -> Option<&'static dyn Driver> {
        self.drivers
            .iter()
            .copied()
            .find(|d| d.compatibles().contains(&dev.compatible))
    }
}

/// 获取总线实例。
pub fn bus() -> &'static Bus {
    BUS.get().expect("bus not initialized")
}

/// 按设备实例类型查找设备（遍历 devices，downcast instance）。
///
/// 返回第一个 probe 成功并挂载该类型实例的设备。
pub fn find<T: 'static>() -> Option<&'static T> {
    bus().devices.read().iter().find_map(|d| d.instance::<T>())
}
