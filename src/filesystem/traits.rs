// 文件系统 trait — 统一文件操作接口
//
// 单个 File trait 描述文件能力（Linux `struct file_operations` 的对应物）。
// 方法带 offset 参数：偏移由 VFS 层（filetable）维护，每次调用传入，
// 设备按 offset 定位（块设备/普通文件）或忽略（字节设备）。
// 不支持的操作用默认实现降级为 FileError::NotSupported，实现者按能力覆盖。

use core::fmt;

// ── Error ─────────────────────────────────────────────────

/// 文件系统错误码。
///
/// 遵循 `std::io::Error` 的模式——模块级别 `Error` + `Result<T>` 别名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // PermissionDenied..NotDirectory 为完整错误码预留，Display 已覆盖
pub enum FileError {
    /// 文件/目录不存在
    NotFound,
    /// 操作不被该文件类型支持（如对字节设备 seek）
    NotSupported,
    /// fd 无效或已关闭
    InvalidFd,
    /// 权限不足（预留，当前始终允许）
    PermissionDenied,
    /// I/O 错误
    IoError,
    /// 文件结束
    Eof,
    /// 非阻塞读/写：操作暂不可完成（如流设备无数据就绪）
    WouldBlock,
    /// 非法参数
    InvalidArg,
    /// 路径中某组件不是目录
    NotDirectory,
}

/// 文件系统操作结果。
pub type Result<T> = core::result::Result<T, FileError>;

// ── File ──────────────────────────────────────────────────

/// 文件能力 — 统一文件操作接口。
///
/// 实现者（设备驱动、devfs 节点、未来的文件系统）按自身能力覆盖方法；
/// 未覆盖的默认返回 [`FileError::NotSupported`]。
///
/// `offset` 由 VFS 层维护（`filetable` 的 `OpenFile::offset`），每次调用传入：
/// 块设备/普通文件据此定位，字节设备（console 等流设备）忽略。
/// 因此文件定位（seek）不是设备能力——它只是 VFS 层修改偏移的操作。
pub trait File: Send + Sync {
    /// 从 `offset` 读取最多 `buf.len()` 字节，返回实际读取字节数（0 表示 EOF）。
    ///
    /// 流式设备（console 等）无 EOF；无数据就绪时返回
    /// [`FileError::WouldBlock`]（非阻塞语义，调用方自行重试或等待中断）。
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize> {
        Err(FileError::NotSupported)
    }

    /// 从 `offset` 写入 `buf` 的全部字节，返回实际写入字节数。
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize> {
        Err(FileError::NotSupported)
    }

    /// 计算新的绝对偏移（Linux `file_operations::llseek` 对应物）。
    ///
    /// `current` 为 VFS 层维护的当前偏移（`OpenFile::offset`），调用方为 `filetable::seek`，
    /// 返回值写回 `OpenFile::offset`。
    /// 默认实现处理 `Start`/`Current` 的算术；`End` 需要实现者覆盖（读取自身末尾），
    /// 流式设备（console 等）默认返回 [`FileError::NotSupported`]。
    #[allow(dead_code)] // filetable::seek 未接入，VFS 偏移 API 预留
    fn seek(&self, pos: SeekFrom, current: usize) -> Result<usize> {
        match pos {
            SeekFrom::Start(off) => Ok(off),
            SeekFrom::Current(delta) => {
                let new = (current as isize).wrapping_add(delta);
                if new < 0 {
                    Err(FileError::InvalidArg)
                } else {
                    Ok(new as usize)
                }
            }
            SeekFrom::End(_) => Err(FileError::NotSupported),
        }
    }

    /// 设备控制命令（Linux ioctl 语义）— `cmd` 命令码，`arg` 参数，语义由设备定义。
    #[allow(dead_code)] // ioctl 语义预留
    fn control(&self, _cmd: u32, _arg: usize) -> Result<isize> {
        Err(FileError::NotSupported)
    }
}

// ── SeekFrom ──────────────────────────────────────────────

/// 文件偏移定位方式（`seek` 使用，偏移始终由 VFS 层维护）。
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // File::seek 默认实现 match 覆盖；变体待调用方构造（VFS seek 预留）
pub enum SeekFrom {
    /// 从文件开头偏移
    Start(usize),
    /// 从当前偏移移动
    Current(isize),
    /// 从文件末尾偏移
    End(isize),
}

// ── OpenFlags ─────────────────────────────────────────────

/// 打开文件标志位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(u8);

#[allow(dead_code)] // RDWR/is_readable/is_writable 为 POSIX 语义预留
impl OpenFlags {
    /// 只读
    pub const READ: OpenFlags = OpenFlags(1 << 0);
    /// 只写
    pub const WRITE: OpenFlags = OpenFlags(1 << 1);
    /// 读写
    pub const RDWR: OpenFlags = OpenFlags(Self::READ.0 | Self::WRITE.0);

    /// 是否包含读权限。
    #[inline]
    pub fn is_readable(self) -> bool {
        self.0 & Self::READ.0 != 0
    }

    /// 是否包含写权限。
    #[inline]
    pub fn is_writable(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }
}

// ── fmt::Display for Error ────────────────────────────────

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::NotFound => write!(f, "not found"),
            FileError::NotSupported => write!(f, "not supported"),
            FileError::InvalidFd => write!(f, "invalid fd"),
            FileError::PermissionDenied => write!(f, "permission denied"),
            FileError::IoError => write!(f, "i/o error"),
            FileError::Eof => write!(f, "eof"),
            FileError::WouldBlock => write!(f, "would block"),
            FileError::InvalidArg => write!(f, "invalid argument"),
            FileError::NotDirectory => write!(f, "not a directory"),
        }
    }
}
