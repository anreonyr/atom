// block/ — 块设备驱动角色目录
//
// 一个型号一个文件，只实现 hal::block::BlockDevice 能力 + Driver（Linux
// block_device_operations 对应物）；File（VFS）视图与 /dev/block0 注册在
// file::io::block（服务集成层），driver 层不碰 File 类型（同 uart 不碰终端语义）。

pub mod virtio_blk;

use crate::driver::traits::Driver;

/// 本角色目录的驱动表（hub::drivers 聚合）。
pub const DRIVERS: &[&dyn Driver] = &[virtio_blk::DRIVER];
