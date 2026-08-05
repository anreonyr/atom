// 设备模型 — Device 结构 + DTB 设备发现
//
// Device 是中枢上的设备描述：资源信息（compatible/reg/irq）+ 生命周期状态。
// hub 将 Device 匹配给 Driver；probe 成功后的实例挂在设备的 instance 字段上
// （Linux dev_set_drvdata 的对应物），运行期通过 `hub::find` 按类型查询。
//
// 状态字段由 SpinLock 保护（Cell 不 Sync，无法放进 static Bus）。
// 锁关闭中断，probe 与中断上下文查询之间安全。

use core::any::Any;

use alloc::vec::Vec;

use crate::driver::traits::Driver;
use crate::lock::SpinLock;
use crate::memory::addr::PhysAddr;
use crate::platform::Dtb;

/// 设备生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 尚未 probe
    Unbound,
    /// probe 返回 Deferred，等待重试
    Deferred,
    /// probe 成功，已绑定驱动
    Bound,
    /// 无驱动认识该设备（不阻塞，同 Linux 未绑定设备）
    Unsupported,
}

/// 设备状态 — 受单锁保护。
struct DeviceStatus {
    state: DeviceState,
    driver: Option<&'static dyn Driver>,
    instance: Option<&'static (dyn Any + Send + Sync)>,
}

/// 总线上的设备。
///
/// 资源字段（compatible/base/size/interrupt）供驱动 probe 读取；
/// 状态字段由 hub 管理（probe 编排），实例由驱动 probe 挂载。
pub struct Device {
    /// 匹配的 compatible 字符串
    pub compatible: &'static str,
    /// MMIO 基址 (reg[0])
    pub base: PhysAddr,
    /// MMIO 区域大小 (reg[0])
    pub size: usize,
    /// 中断号（如有）
    pub interrupt: Option<u32>,
    /// 生命周期状态 + 绑定 + 实例（Linux dev->driver / dev_set_drvdata 语义）
    status: SpinLock<DeviceStatus>,
}

impl Device {
    pub(crate) fn new(
        compatible: &'static str,
        base: PhysAddr,
        size: usize,
        interrupt: Option<u32>,
    ) -> Self {
        Self {
            compatible,
            base,
            size,
            interrupt,
            status: SpinLock::new(DeviceStatus {
                state: DeviceState::Unbound,
                driver: None,
                instance: None,
            }),
        }
    }

    /// 当前生命周期状态。
    pub fn state(&self) -> DeviceState {
        self.status.lock().state
    }

    /// 已绑定的驱动（未绑定返回 None）。
    ///
    /// 对应 Linux `dev->driver`；由 [`Hub::bound_devices`](crate::driver::hub::Hub::bound_devices)
    /// 消费（boot 日志报告绑定结果）。
    pub fn driver(&self) -> Option<&'static dyn Driver> {
        self.status.lock().driver
    }

    /// 设置生命周期状态（hub 专用）。
    pub fn set_state(&self, s: DeviceState) {
        self.status.lock().state = s;
    }

    /// 设置绑定驱动（hub 在 probe 成功后调用，Linux dev->driver）。
    pub fn set_driver(&self, drv: &'static dyn Driver) {
        self.status.lock().driver = Some(drv);
    }

    /// 挂载设备实例（驱动 probe 内调用，Linux dev_set_drvdata 语义）。
    pub fn set_instance<T: 'static + Send + Sync>(&self, data: &'static T) {
        self.status.lock().instance = Some(data as &'static (dyn Any + Send + Sync));
    }

    /// 取回设备实例（`hub::find` 内部用），按具体类型 downcast。
    pub fn instance<T: 'static>(&self) -> Option<&'static T> {
        let status = self.status.lock();
        status
            .instance
            .and_then(|d| (d as &dyn Any).downcast_ref::<T>())
    }
}

// ── DTB 设备发现 ─────────────────────────────────────────

/// 执行设备发现：从 DTB 解析设备列表，失败时使用回退。
///
/// 由 `hub::init()` 在 allocator 就绪后调用一次。
pub fn probe() -> Vec<Device> {
    match crate::platform::config::dtb() {
        Some(dtb) => parse(dtb),
        None => fallback_devices(),
    }
}

/// 从校验过的 DTB 句柄解析设备列表。
fn parse(dtb: &Dtb) -> Vec<Device> {
    let mut devices = Vec::new();
    for node in dtb.walk() {
        if let Some((base, size)) = node.property_reg(dtb, 0)
            && size > 0
            && let Some(compatible) = node.property_string(dtb, "compatible")
        {
            // SAFETY: DTB physical memory is reserved by OpenSBI and never freed;
            // the &str reference into it remains valid for the entire kernel lifetime.
            let compatible: &'static str = unsafe { core::mem::transmute(compatible) };
            let interrupt = node.property_u32(dtb, "interrupts");
            devices.push(Device::new(
                compatible,
                PhysAddr::from_raw(base as usize),
                size as usize,
                interrupt,
            ));
        }
    }
    devices
}

/// DTB 不可用时的回退设备列表。
fn fallback_devices() -> Vec<Device> {
    Vec::from([
        Device::new(
            "sifive,plic-1.0.0",
            PhysAddr::from_raw(crate::platform::qemu_virt::PLIC_BASE),
            crate::platform::qemu_virt::PLIC_SIZE,
            None,
        ),
        Device::new(
            "ns16550a",
            PhysAddr::from_raw(crate::platform::qemu_virt::UART_BASE),
            crate::platform::qemu_virt::UART_SIZE,
            Some(crate::platform::qemu_virt::UART_INTERRUPT),
        ),
        Device::new(
            "sifive,clint0",
            PhysAddr::from_raw(crate::platform::qemu_virt::CLINT_BASE),
            crate::platform::qemu_virt::CLINT_SIZE,
            None,
        ),
    ])
}
