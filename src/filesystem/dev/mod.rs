// devfs — 设备文件系统工厂
//
// create_devfs() 构建 /dev 子树并返回根 Inode，引导期调用一次。
// 设备实例经 crate::uart 注册表（uart::all）获取（probe 完成后注册），
// 注入 Inode 的 file 字段（Linux 驱动注册 cdev、VFS 通过 fops 操作的对应物）。
// 依赖方向：filesystem 依赖契约层（file.rs / uart.rs），不依赖 driver。

pub mod log;
pub mod null;
pub mod zero;

use crate::console::read::Console;
use crate::filesystem::dev::log::LOG;
use crate::filesystem::dev::null::NULL;
use crate::filesystem::dev::zero::ZERO;
use crate::filesystem::inode::{Inode, InodeBuilder, InodeType};

/// console 标准流节点 — 零尺寸静态实例（Console 无状态）。
/// /dev/stdin 与 /dev/stdout 挂同一实例：read 走 preferred 输入源、
/// write 走 preferred 输出源（stdin/stdout 统一视图）。
static CONSOLE: Console = Console;

/// 构建 /dev 设备文件系统子树。
///
/// 创建以下命名空间：
/// ```text
/// /                (Directory)
/// └── dev          (Directory)
///     ├── log      (ByteDevice → LogDev)
///     ├── null     (ByteDevice → NullDev)
///     ├── stdin    (ByteDevice → Console，console 输入源)
///     ├── stdout   (ByteDevice → Console，console 输出源)
///     └── zero     (ByteDevice → ZeroDev)
/// ```
///
/// 返回根 Inode。调用方通过 `set_root()` 注册为全局命名空间根。
pub fn create_devfs() -> &'static Inode {
    let log = InodeBuilder::new("log", InodeType::ByteDevice)
        .with_file(&LOG)
        .build();

    let null = InodeBuilder::new("null", InodeType::ByteDevice)
        .with_file(&NULL)
        .build();

    let zero = InodeBuilder::new("zero", InodeType::ByteDevice)
        .with_file(&ZERO)
        .build();

    let stdin = InodeBuilder::new("stdin", InodeType::ByteDevice)
        .with_file(&CONSOLE)
        .build();

    let stdout = InodeBuilder::new("stdout", InodeType::ByteDevice)
        .with_file(&CONSOLE)
        .build();

    // /dev 目录——console 标准流节点（stdin/stdout 挂同一 Console）与内建设备
    let mut dev = InodeBuilder::new("dev", InodeType::Directory);
    dev = dev
        .with_child(log)
        .with_child(null)
        .with_child(stdin)
        .with_child(stdout)
        .with_child(zero);

    // 根 /
    InodeBuilder::new("/", InodeType::Directory)
        .with_child(dev.build())
        .build()
}
