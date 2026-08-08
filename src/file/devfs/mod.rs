// devfs — 设备文件系统工厂（纯枚举器）
//
// create_devfs() 枚举复用器注册表（file::registry）构建 /dev 子树并返回根
// Inode，引导期调用一次。设备注册点：
//   - 内建：null/zero（本模块幂等注册）
//   - 标准流：init.rs 注册 /dev/stdin、/dev/stdout（io::stdio::STDIN/STDOUT）
//   - UART：io::uart::register 注册 /dev/consoleN（probe 时）
//
// 复用器模型：本层不反向依赖任何提供方（log/driver），只消费注册表
// （Linux 驱动注册 cdev、VFS 通过 fops 操作的对应物）。
//
// /dev/log 暂移除（file 域断环：log → file::io 输出依赖，file 不再反向依赖
// log；LogDev 预留于 log 域，未来经 registry 恢复注册）。

pub mod null;
pub mod zero;

use crate::file::devfs::null::NULL;
use crate::file::devfs::zero::ZERO;
use crate::file::registry;
use crate::file::vfs::inode::{Inode, InodeBuilder, InodeType};

/// 构建 /dev 设备文件系统子树。
///
/// 注册内建设备（幂等），随后枚举注册表构建命名空间：
/// ```text
/// /                (Directory)
/// └── dev          (Directory)
///     ├── null     (ByteDevice → NullDev)
///     ├── stdin    (ByteDevice → Stdin，console 输入源)
///     ├── stdout   (ByteDevice → Stdout，console 输出源)
///     ├── zero     (ByteDevice → ZeroDev)
///     └── consoleN (ByteDevice → UartFile，每个已注册 UART)
/// ```
///
/// 返回根 Inode。调用方通过 `set_root()` 注册为全局命名空间根。
pub fn create_devfs() -> &'static Inode {
    // 内建设备注册（幂等；stdin/stdout 由 init.rs 注册，UART 由 io::uart 注册）
    let _ = registry::register("/dev/null", &NULL);
    let _ = registry::register("/dev/zero", &ZERO);

    // 枚举注册表构建 /dev 节点（条目路径统一 "/dev/<name>"）
    let mut dev = InodeBuilder::new("dev", InodeType::Directory);
    for entry in registry::all() {
        let name = entry.name.strip_prefix("/dev/").unwrap_or(entry.name);
        dev = dev.with_child(
            InodeBuilder::new(name, InodeType::ByteDevice)
                .with_file(entry.file)
                .build(),
        );
    }

    // 根 /
    InodeBuilder::new("/", InodeType::Directory)
        .with_child(dev.build())
        .build()
}
