// devfs — 设备文件系统工厂
//
// create_devfs() 构建 /dev 子树并返回根 Inode，引导期调用一次。
// 设备实例通过 serial 注册表（serial::all）获取（probe 完成后挂载），
// 注入 Inode 的 file 字段（Linux 驱动注册 cdev、VFS 通过 fops 操作的对应物）。

pub mod log;
pub mod null;
pub mod zero;

use crate::filesystem::dev::log::LOG;
use crate::filesystem::dev::null::NULL;
use crate::filesystem::dev::zero::ZERO;
use crate::filesystem::inode::{Inode, InodeBuilder, InodeType};

/// 构建 /dev 设备文件系统子树。
///
/// 创建以下命名空间：
/// ```text
/// /                (Directory)
/// └── dev          (Directory)
///     ├── console0 (ByteDevice → UART[0])
///     ├── console1 (ByteDevice → UART[1]，多 UART 时按发现顺序编号)
///     ├── log      (ByteDevice → LogDev)
///     ├── null     (ByteDevice → NullDev)
///     └── zero     (ByteDevice → ZeroDev)
/// ```
///
/// 返回根 Inode。调用方通过 `set_root()` 注册为全局命名空间根。
pub fn create_devfs() -> &'static Inode {
    // 枚举所有 UART（serial 注册表，跨型号）：每个 UART 一个 consoleN 节点
    let uarts = crate::driver::serial::all();
    let log = InodeBuilder::new("log", InodeType::ByteDevice)
        .with_file(&LOG)
        .build();

    let null = InodeBuilder::new("null", InodeType::ByteDevice)
        .with_file(&NULL)
        .build();

    let zero = InodeBuilder::new("zero", InodeType::ByteDevice)
        .with_file(&ZERO)
        .build();

    // /dev 目录——UART 数量由设备发现决定，每个为 /dev/consoleN
    let mut dev = InodeBuilder::new("dev", InodeType::Directory);
    for (i, uart) in uarts.iter().enumerate() {
        let name: &'static str = alloc::format!("console{}", i).leak();
        dev = dev.with_child(
            InodeBuilder::new(name, InodeType::ByteDevice)
                .with_file(uart.file)
                .build(),
        );
    }
    dev = dev.with_child(log).with_child(null).with_child(zero);

    // 根 /
    InodeBuilder::new("/", InodeType::Directory)
        .with_child(dev.build())
        .build()
}
