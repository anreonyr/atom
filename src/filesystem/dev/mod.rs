// devfs — 设备文件系统工厂
//
// create_devfs() 构建 /dev 子树并返回根 Inode，引导期调用一次。

pub mod console;
pub mod null;
pub mod zero;

use crate::filesystem::dev::console::CONSOLE;
use crate::filesystem::dev::null::NULL;
use crate::filesystem::dev::zero::ZERO;
use crate::filesystem::inode::{Inode, InodeBuilder, InodeType};
use crate::filesystem::traits::{FileRead, FileWrite};

/// 构建 /dev 设备文件系统子树。
///
/// 创建以下命名空间：
/// ```text
/// /                (Directory)
/// └── dev          (Directory)
///     ├── console  (ByteDevice → UART)
///     ├── null     (ByteDevice → NullDev)
///     └── zero     (ByteDevice → ZeroDev)
/// ```
///
/// 返回根 Inode。调用方通过 `set_root()` 注册为全局命名空间根。
pub fn create_devfs() -> &'static Inode {
    // 设备 Inode
    let console = InodeBuilder::new("console", InodeType::ByteDevice)
        .with_read(&CONSOLE as &dyn FileRead)
        .with_write(&CONSOLE as &dyn FileWrite)
        .build();

    let null = InodeBuilder::new("null", InodeType::ByteDevice)
        .with_read(&NULL as &dyn FileRead)
        .with_write(&NULL as &dyn FileWrite)
        .build();

    let zero = InodeBuilder::new("zero", InodeType::ByteDevice)
        .with_read(&ZERO as &dyn FileRead)
        .with_write(&ZERO as &dyn FileWrite)
        .build();

    // /dev 目录
    let dev_dir = InodeBuilder::new("dev", InodeType::Directory)
        .with_child(console)
        .with_child(null)
        .with_child(zero)
        .build();

    // 根 /
    InodeBuilder::new("", InodeType::Directory)
        .with_child(dev_dir)
        .build()
}
