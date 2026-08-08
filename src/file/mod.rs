// file — 文件域（多路复用器框架）
//
// 顶层契约（ops）约束全部文件实现：VFS 框架（vfs/）、设备文件系统（devfs/）、
// 标准流（io/）与未来具体文件系统（如 fat32/）都实现 file::ops 的 File/Read/Write。
// 本层是复用器：注册（registry）与分发（vfs::filetable）——不反向依赖任何
// 提供方（log/driver 只注册、只消费契约）。
//
// 文件能力契约定义在 ops 子模块（零依赖原子叶子——契约随域归位）；
// 外部消费方直连 `crate::file::ops`。

pub mod devfs;
pub mod fs;
#[macro_use]
pub mod io;
pub mod ops;
pub mod registry;
pub mod vfs;

// 重新导出文件契约与 VFS 便利面（demos 等经 `crate::file::X` 消费）
#[allow(unused_imports)]
pub use ops::{File, FileError, OpenFlags, Result, SeekFrom};
#[allow(unused_imports)]
pub use vfs::filetable::Stat;
#[allow(unused_imports)]
pub use vfs::filetable::{
    close, control, create, fstat, open, read, readdir, seek, set_root, write,
};
