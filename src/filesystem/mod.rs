// 虚拟文件系统 — 内核文件抽象层
//
// 提供统一的文件操作接口，将硬件设备和虚拟设备暴露为文件系统节点。
// trait 对象在 VFS 层（Inode），不在 hub 层——hub 只存具体类型。

pub mod dev;
pub mod filetable;
pub mod inode;
pub mod traits;

// 重新导出常用类型和函数（bin crate 内部分暂未使用，作为公共 API 面保留）
#[allow(unused_imports)]
pub use filetable::{close, control, open, read, seek, set_root, write};
#[allow(unused_imports)]
pub use traits::{File, FileError, OpenFlags, Result, SeekFrom};
