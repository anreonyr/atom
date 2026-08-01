// 设备中枢 — 设备发现、驱动匹配、probe 编排、设备实例查询
//
// Hub 承担 Linux `bus_type` + 子系统注册表的职责，是驱动子系统的"中枢"：
//   引导期：`device::probe()` 发现设备 → 按 compatible 匹配驱动 → deferred probe
//   运行期：`hub::find::<T>()` 按具体类型查询设备实例（instance downcast）
//
// 命名取 hub 而非 bus：当前平台无真实总线概念（设备平铺自 DTB），
// hub 是"设备中枢"；未来引入具体总线（virtio-mmio、PCI 等）时，
// 总线概念回到各自的 bus 实现，hub 继续作为聚合编排层。

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::driver::device::{self, Device, DeviceState};
use crate::driver::traits::{Driver, DriverError};
use crate::lock::{OnceLock, RwLock};

/// 设备中枢。
pub struct Hub {
    /// 发现到的设备（引导期填充，运行期只读）
    devices: RwLock<Vec<Device>>,
    /// 驱动表（各角色目录 `DRIVERS` 的聚合，静态）
    drivers: &'static [&'static dyn Driver],
}

static HUB: OnceLock<&'static Hub> = OnceLock::new();

/// 初始化设备中枢：发现设备 → 匹配驱动 → deferred probe。
///
/// 必须先 `HUB.set` 再 `probe_all`——驱动 probe 内会调 `hub::find`。
pub fn init() -> Result<(), DriverError> {
    let hub = Box::leak(Box::new(Hub {
        devices: RwLock::new(device::probe()),
        drivers: drivers(),
    }));
    if HUB.set(hub).is_err() {
        panic!("hub already initialized");
    }
    hub.probe()
}

/// 聚合各角色目录（serial/controller）的驱动表。
fn drivers() -> &'static [&'static dyn Driver] {
    static DRIVERS: OnceLock<Vec<&'static dyn Driver>> = OnceLock::new();
    DRIVERS.get_or_init(|| {
        super::uart::DRIVERS
            .iter()
            .chain(super::controller::DRIVERS.iter())
            .copied()
            .collect()
    })
}

impl Hub {
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
                // RwLock guard 释放后 Vec 仍存储在 Hub 内，引用保持有效。
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

    /// 设备状态统计 — 供 boot 日志报告设备发现结果。
    ///
    /// 返回 `(total, bound, unsupported)`：发现总数、成功绑定数、无驱动支持数。
    /// 剩余设备为 Deferred（probe 编排后不应残留，异常时用于诊断）。
    pub fn device_summary(&self) -> (usize, usize, usize) {
        let devices = self.devices.read();
        let total = devices.len();
        let bound = devices
            .iter()
            .filter(|d| d.state() == DeviceState::Bound)
            .count();
        let unsupported = devices
            .iter()
            .filter(|d| d.state() == DeviceState::Unsupported)
            .count();
        (total, bound, unsupported)
    }

    /// 已绑定设备 `(compatible, 驱动名)` 列表（按设备发现顺序）。
    ///
    /// 供 boot 日志逐设备报告绑定结果；驱动名来自 [`Driver::name`]。
    pub fn bound_devices(&self) -> Vec<(&'static str, &'static str)> {
        self.devices
            .read()
            .iter()
            .filter_map(|d| d.driver().map(|drv| (d.compatible, drv.name())))
            .collect()
    }
}

/// 获取设备中枢实例。
pub fn get() -> &'static Hub {
    HUB.get().expect("hub not initialized")
}

/// 按设备实例类型查找设备（遍历 devices，downcast instance）。
///
/// 返回第一个 probe 成功并挂载该类型实例的设备。
pub fn find<T: 'static>() -> Option<&'static T> {
    get().devices.read().iter().find_map(|d| d.instance::<T>())
}
