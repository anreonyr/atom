// 设备驱动子系统 — hub/device/driver 模型
//
// 模块结构：
//   traits.rs   — Driver trait + DriverError（数据式驱动抽象）
//   device.rs   — Device + DeviceState + DTB 设备发现
//   hub.rs      — Hub：设备发现 + 匹配 + probe 编排 + 实例查询
//   serial/     — 串口设备驱动（uart16550，按型号分文件）
//   controller/ — 控制器设备驱动（plic, clint）

pub mod controller;
pub mod device;
pub mod hub;
pub mod traits;
pub mod uart;

// Driver 为对外 API（驱动模块内部经 traits::Driver 引用），此处 re-export 供外部使用。
#[allow(unused_imports)]
pub use traits::{Driver, DriverError};

use crate::driver::device::Device;

/// 映射设备 MMIO 区域到内核地址空间（驱动 probe 用）。
///
/// size 取整到 PAGE_SIZE 整倍数（兼容 DTB reg size 非对齐），按设备标志
/// （V|R|W|A|D|G，G 保证跨地址空间可见）映射到内核地址空间。取整与设备
/// 标志是「设备映射」的领域知识，集中在此一处，驱动 probe 直接调用。
///
/// # Safety
///
/// 调用方需确保 `dev.base`/`dev.size` 描述有效的 MMIO 区域。
///
/// # Errors
///
/// 内核地址空间未初始化或映射失败时返回 [`DriverError::MapFailed`]。
pub(crate) unsafe fn map_mmio(dev: &Device) -> Result<(), DriverError> {
    use crate::memory::{
        addr::VirtAddr,
        allocator::page,
        entry::PteFlags,
        space::kernel_space,
    };

    let dev_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D | PteFlags::G;

    let guard = kernel_space();
    let ks = guard.as_ref().ok_or(DriverError::MapFailed(dev.compatible))?;

    // AddressSpace::map 要求 vaddr/paddr/size 全部页对齐；非对齐 size 在此取整
    let aligned_size = if dev.size > 0 {
        (dev.size + crate::memory::PAGE_SIZE - 1) & !(crate::memory::PAGE_SIZE - 1)
    } else {
        crate::memory::PAGE_SIZE
    };
    ks.map(
        VirtAddr::from_raw(dev.base.as_usize()),
        dev.base,
        aligned_size,
        dev_flags,
        page::allocator(),
    )
    .map_err(|_| DriverError::MapFailed(dev.compatible))
}

/// 设备驱动子系统初始化：设备中枢发现设备 → 匹配驱动 → deferred probe。
pub(crate) fn init() -> Result<(), DriverError> {
    hub::init()
}
