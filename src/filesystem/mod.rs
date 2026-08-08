// 虚拟文件系统 — 内核文件抽象层
//
// 提供统一的文件操作接口，将硬件设备和虚拟设备暴露为文件系统节点。
// 文件能力契约（File trait 等）定义在本目录 `ops` 子模块（零依赖原子
// 叶子——契约随域归位，本层其他模块消费它）；UART 设备经 crate::uart
// 注册表获取——filesystem 不依赖 driver。
// trait 对象在 VFS 层（Inode），不在 hub 层——hub 只存具体类型。

pub mod dev;
pub mod filetable;
pub mod inode;
pub mod ops;

// 重新导出文件契约（外部消费方直连 `crate::filesystem::ops` 亦可；
// 本层重导出保留 `filesystem::X` 公共面。注意：console::read 因
// filesystem::dev → console::read 反向依赖，必须直连 ops 叶子防环）
#[allow(unused_imports)]
pub use ops::{File, FileError, OpenFlags, Result, SeekFrom};
#[allow(unused_imports)]
pub use filetable::{close, control, open, read, seek, set_root, write};
