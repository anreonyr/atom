// 文件能力契约层 — File trait + 错误码 + 标志位
//
// 本模块是 VFS（filesystem/）与设备驱动（driver/）共同依赖的最底层契约，
// 只定义"文件操作长什么样"（协议），不承载任何实现/状态/组织：
//   - driver 的 UART 等设备实现 `File`（经 uart.rs 适配，或 devfs 节点直接实现）
//   - filesystem 的 Inode/filetable/devfs 消费 `File`（文件抽象）
//   - 双方都不互相依赖，只依赖本模块（Linux `include/linux/fs.h` 中
//     `struct file_operations` 的对应物）
//
// 单个 File trait 描述文件能力。方法带 offset 参数：偏移由 VFS 层
// （filetable）维护，每次调用传入，设备按 offset 定位（块设备/普通文件）
// 或忽略（字节设备）。不支持的操作用默认实现降级为 FileError::NotSupported，
// 实现者按能力覆盖。

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
    /// 访问模式（fd 是否以读方式打开）由 VFS 层（`filetable::read`）强制，
    /// 实现者不检查。
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize> {
        Err(FileError::NotSupported)
    }

    /// 从 `offset` 写入 `buf` 的全部字节，返回实际写入字节数。
    ///
    /// 访问模式（fd 是否以写方式打开）由 VFS 层（`filetable::write`）强制，
    /// 实现者不检查。
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize> {
        Err(FileError::NotSupported)
    }

    /// 计算新的绝对偏移（Linux `file_operations::llseek` 对应物）。
    ///
    /// `current` 为 VFS 层维护的当前偏移（`OpenFile::offset`），调用方为 `filetable::seek`，
    /// 返回值写回 `OpenFile::offset`。
    /// 默认实现处理 `Start`/`Current` 的算术；`End` 需要实现者覆盖（读取自身末尾），
    /// 流式设备（console 等）默认返回 [`FileError::NotSupported`]。
    ///
    /// 已接通：main.rs demo 对 `/dev/log` 演示 `seek(Start(0))` 重读。
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
    ///
    /// 调用链：envcall CONTROL（1005）→ `filetable::control` → 本方法。
    /// 默认返回 [`FileError::NotSupported`]，实现者按设备能力覆盖。
    fn control(&self, _cmd: u32, _arg: usize) -> Result<isize> {
        Err(FileError::NotSupported)
    }

    /// 文件大小（字节）— fstat 元数据查询（Linux `i_size` 对应物）。
    ///
    /// 默认 0：字节设备（console 等）与目录合法为 0（POSIX st_size 对字符设备
    /// 即 0）；普通文件/块设备实现者覆盖上报真实大小（M4 文件系统）。调用链：
    /// envcall FSTAT（1007）→ `filetable::fstat` → 本方法。
    fn size(&self) -> usize {
        0
    }
}

// ── Read / Write（流契约，std::io 对应物）───────────────

/// 流读取能力 — `std::io::Read` 对应物。
///
/// 无 offset（流设备不定位）；阻塞/非阻塞由实现者决定（如 io::stdio 的
/// `Stdin`：File 视图非阻塞返回 WouldBlock、句柄方法阻塞等待）。
/// 与 [`File::read`] 的区别：`File` 是 VFS 层带 offset 的文件契约，
/// `Read` 是面向流句柄的用户契约（std::io 形态）。
#[allow(dead_code)] // 预留契约：当前消费方走固有方法，trait 面供未来流设备/fat32 使用
pub trait Read {
    /// 读取最多 `buf.len()` 字节，返回实际读取字节数。
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// 读取单字节（阻塞/非阻塞语义同 [`Read::read`]）。
    fn read_byte(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read(&mut b)?;
        Ok(b[0])
    }
}

/// 流写入能力 — `std::io::Write` 对应物。
#[allow(dead_code)] // 预留契约：当前消费方走固有方法，trait 面供未来流设备/fat32 使用
pub trait Write {
    /// 写入 `buf`，返回实际写入字节数。
    fn write(&mut self, buf: &[u8]) -> Result<usize>;

    /// 冲刷缓冲 — 无缓冲层实现为 no-op（默认）。
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    /// 写入全部字节（循环直到写完；0 字节写入视为 [`FileError::IoError`] 防死循环）。
    fn write_all(&mut self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            let n = self.write(buf)?;
            if n == 0 {
                return Err(FileError::IoError);
            }
            buf = &buf[n..];
        }
        Ok(())
    }

    /// 格式化写入（`write!`/`writeln!` 宏经 `fmt::Write` 适配的底层）。
    ///
    /// 默认实现经本地 adapter 把格式化结果逐片段转发给 [`Write::write_all`]
    /// （与 `std::io::Write::write_fmt` 同构，无堆缓冲）。
    fn write_fmt(&mut self, args: fmt::Arguments) -> Result<()> {
        struct Adapter<'a, W: Write + ?Sized>(&'a mut W);
        impl<W: Write + ?Sized> fmt::Write for Adapter<'_, W> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0.write_all(s.as_bytes()).map_err(|_| fmt::Error)
            }
        }
        fmt::write(&mut Adapter(self), args).map_err(|_| FileError::IoError)
    }
}

// ── SeekFrom ──────────────────────────────────────────────

/// 文件偏移定位方式（`seek` 使用，偏移始终由 VFS 层维护）。
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // End 字段默认实现不读（需实现者覆盖读取自身末尾）；由 envcall SEEK（1004）whence 2 构造
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
