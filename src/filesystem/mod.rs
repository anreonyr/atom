// 虚拟文件系统 — 内核文件抽象层
//
// 提供统一的文件操作接口，将硬件设备和虚拟设备暴露为文件系统节点。
// trait 对象在 VFS 层（Inode），不在 hub 层——hub 只存具体类型。

pub mod dev;
pub mod filetable;
pub mod inode;
pub mod traits;

// 重新导出常用类型和函数
pub use filetable::{close, control, lseek, open, read, set_root, write};
pub use traits::{
    FileControl, FileError, FileRead, FileSeek, FileWrite, OpenFlags, Result, SeekFrom,
};
