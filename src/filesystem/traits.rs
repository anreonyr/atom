// 文件系统 trait — 定义文件操作的基本能力
//
// 四个独立 trait，设备按需实现。遵循 Rust 标准库模式：
// 每个 trait 代表一种独立能力，不强制实现不需要的操作。

use core::fmt;

// ── Error ─────────────────────────────────────────────────

/// 文件系统错误码。
///
/// 遵循 `std::io::Error` 的模式——模块级别 `Error` + `Result<T>` 别名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// 文件/目录不存在
    NotFound,
    /// 操作不被该文件类型支持（如对 ByteDevice seek）
    NotSupported,
    /// fd 无效或已关闭
    InvalidFd,
    /// 权限不足（预留，当前始终允许）
    PermissionDenied,
    /// I/O 错误
    IoError,
    /// 文件结束
    Eof,
    /// 非法参数
    InvalidArg,
    /// 路径中某组件不是目录
    NotDirectory,
}

/// 文件系统操作结果。
pub type Result<T> = core::result::Result<T, Error>;

// ── FileRead ──────────────────────────────────────────────

/// 文件读取 trait — 字节设备、块设备、普通文件实现。
///
/// 读取最多 `buf.len()` 字节，返回实际读取的字节数。
/// 返回 0 表示 EOF。
pub trait FileRead: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> Result<usize>;
}

// ── FileWrite ─────────────────────────────────────────────

/// 文件写入 trait — 字节设备、块设备、普通文件实现。
///
/// 写入 `buf` 的全部字节，返回实际写入的字节数。
pub trait FileWrite: Send + Sync {
    fn write(&self, buf: &[u8]) -> Result<usize>;
}

// ── FileSeek ──────────────────────────────────────────────

/// 文件定位 trait — 可随机访问的文件实现（流设备不实现）。
pub trait FileSeek: Send + Sync {
    /// 设置文件偏移，返回新的绝对偏移。
    fn seek(&self, pos: SeekFrom) -> Result<usize>;
}

// ── FileControl ───────────────────────────────────────────

/// 设备控制 trait — 设备文件实现。
///
/// `cmd` 为命令码，`arg` 为参数，语义由设备定义。
/// 返回操作结果（≥0 成功，<0 错误码）。
pub trait FileControl: Send + Sync {
    fn control(&self, cmd: u32, arg: usize) -> Result<isize>;
}

// ── SeekFrom ──────────────────────────────────────────────

/// 文件偏移定位方式。
#[derive(Debug, Clone, Copy)]
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

impl OpenFlags {
    /// 只读
    pub const READ: OpenFlags = OpenFlags(1 << 0);
    /// 只写
    pub const WRITE: OpenFlags = OpenFlags(1 << 1);
    /// 读写
    pub const RDWR: OpenFlags = OpenFlags(Self::READ.0 | Self::WRITE.0);

    /// 是否包含读权限。
    #[inline]
    pub fn readable(self) -> bool {
        self.0 & Self::READ.0 != 0
    }

    /// 是否包含写权限。
    #[inline]
    pub fn writable(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }
}

// ── fmt::Debug for Error ──────────────────────────────────

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::NotSupported => write!(f, "not supported"),
            Error::InvalidFd => write!(f, "invalid fd"),
            Error::PermissionDenied => write!(f, "permission denied"),
            Error::IoError => write!(f, "i/o error"),
            Error::Eof => write!(f, "eof"),
            Error::InvalidArg => write!(f, "invalid argument"),
            Error::NotDirectory => write!(f, "not a directory"),
        }
    }
}
