// devfs — 设备文件系统工厂（纯枚举器）
//
// create_devfs() 枚举复用器注册表（file::registry）构建 /dev 子树并返回根
// Inode，引导期调用一次。设备注册点：
//   - 内建：null/zero（本模块幂等注册）
//   - 标准流：init.rs 注册 /dev/stdin、/dev/stdout（io::stdio::STDIN/STDOUT）
//   - 终端：io::console::register 注册 /dev/consoleN + /dev/uartN（probe 时）
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
/// dev            (Directory)
/// ├── null     (ByteDevice → NullDev)
/// ├── stdin    (ByteDevice → Stdin，console 输入源)
/// ├── stdout   (ByteDevice → Stdout，console 输出源)
/// ├── zero     (ByteDevice → ZeroDev)
/// ├── block0   (BlockDevice → BlockFile，随机块访问)
/// ├── consoleN (ByteDevice → Console，每个已注册终端)
/// ├── uartN    (ByteDevice → RawFile，原始字节流，无终端语义)
/// └── console  (Symlink → 首个 consoleN，preferred 输出目标)
/// ```
///
/// 返回 **dev 子树**（不含根）——根由 init.rs 组装（/dev + /data 多子树），
/// 经 `set_root()` 注册为全局命名空间根。
pub fn create_dev_tree() -> &'static Inode {
    // 内建设备注册（幂等；stdin/stdout 由 init.rs 注册，终端由 io::console 注册）
    let _ = registry::register("/dev/null", &NULL);
    let _ = registry::register("/dev/zero", &ZERO);

    // 枚举注册表构建 /dev 节点（条目路径统一 "/dev/<name>"）；
    // 记录首个 consoleN（首个注册自动 preferred，Linux preferred_console 语义）
    let mut dev = InodeBuilder::new("dev", InodeType::Directory);
    let mut first_console: Option<&'static str> = None;
    for entry in registry::all() {
        let name = entry.name.strip_prefix("/dev/").unwrap_or(entry.name);
        if first_console.is_none() && name.starts_with("console") {
            first_console = Some(name);
        }
        dev = dev.with_child(
            InodeBuilder::new(name, InodeType::ByteDevice)
                .with_file(entry.file)
                .build(),
        );
    }

    // /dev/console → 首个 consoleN 符号链接（preferred 由链接表达；改链接即换
    // 系统控制台）。无终端 → 无链接 → resolve None → print 回落 sbi。
    if let Some(target) = first_console {
        dev = dev
            .with_child(InodeBuilder::new("console", InodeType::Symlink).with_target(target).build());
    }

    dev.build()
}
