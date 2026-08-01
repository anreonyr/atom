// 文件描述符表 — 打开文件管理
//
// FileTable 维护 fd (小整数) → OpenFile 的映射。
// 偏移由 OpenFile 持有，I/O 时作为参数传给 inode 的 File 实现
// （Linux `struct file` 持有 f_pos、调用时传给 fops 的对应物）。
// 当前为全局表（内核单地址空间），后续多进程时每个进程持有一个 FileTable。

use alloc::vec::Vec;

use crate::filesystem::inode::{lookup, Inode};
use crate::file::{FileError, OpenFlags, Result, SeekFrom};
use crate::lock::{OnceLock, RwLock};

// ── OpenFile ──────────────────────────────────────────────

/// 打开的文件描述 — 携带 per-open 状态（对应 Linux `struct file`）。
pub struct OpenFile {
    /// 指向文件系统节点的引用
    pub inode: &'static Inode,
    /// 当前文件偏移（I/O 时传给 File 实现；字节设备忽略）
    pub offset: usize,
    /// 打开标志
    #[allow(dead_code)] // 访问模式检查预留
    pub flags: OpenFlags,
}

// ── FileTable ─────────────────────────────────────────────

/// 文件描述符表。
///
/// `files[i] = Some(OpenFile)` 表示 fd i 已打开；
/// `None` 表示该 fd 已关闭或从未使用（可复用）。
pub struct FileTable {
    files: Vec<Option<OpenFile>>,
}

impl FileTable {
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }
}

// ── 全局状态 ──────────────────────────────────────────────

/// 全局文件描述符表 — RwLock 保护（读多写少）。
// TODO: 多进程后移入进程对象，每进程持有一张表
static FILE_TABLE: RwLock<FileTable> = RwLock::new(FileTable::new());

/// 根 Inode — OnceLock 写一次读多次。
static ROOT_INODE: OnceLock<&'static Inode> = OnceLock::new();

/// 初始化命名空间根节点（引导期调用一次）。
pub fn set_root(root: &'static Inode) {
    if ROOT_INODE.set(root).is_err() {
        crate::warn!("filesystem: root already initialized (set_root called more than once)");
    }
}

// ── 公共 API ──────────────────────────────────────────────

/// 打开指定路径的文件，返回文件描述符。
///
/// fd 分配策略：扫描已关闭的 slot 复用；无空闲时追加到末尾。
pub fn open(path: &str, flags: OpenFlags) -> Result<usize> {
    let root = ROOT_INODE.get().ok_or(FileError::NotFound)?;
    let inode = lookup(root, path).ok_or(FileError::NotFound)?;

    let mut table = FILE_TABLE.write();
    let fd = if let Some(idx) = table.files.iter().position(|f| f.is_none()) {
        idx
    } else {
        let fd = table.files.len();
        table.files.push(None);
        fd
    };

    table.files[fd] = Some(OpenFile {
        inode,
        offset: 0,
        flags,
    });
    Ok(fd)
}

/// 关闭文件描述符。
pub fn close(fd: usize) -> Result<()> {
    let mut table = FILE_TABLE.write();
    if fd >= table.files.len() || table.files[fd].is_none() {
        return Err(FileError::InvalidFd);
    }
    table.files[fd] = None;
    Ok(())
}

/// 从文件描述符读取数据。
///
/// 以当前偏移为参数调用 inode 的 File 实现；读后推进偏移。
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    let n = f.read(file.offset, buf)?;
    file.offset += n;
    Ok(n)
}

/// 向文件描述符写入数据。
///
/// 以当前偏移为参数调用 inode 的 File 实现；写后推进偏移。
pub fn write(fd: usize, buf: &[u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    let n = f.write(file.offset, buf)?;
    file.offset += n;
    Ok(n)
}

/// 设置文件描述符偏移。
///
/// 委托 inode 的 File 实现计算新绝对偏移（Linux `llseek`），写回 `OpenFile::offset`。
/// `Start`/`Current` 由 File 默认实现处理；`End` 需要实现者支持。
/// 已接通：main.rs demo 对 `/dev/log` 演示 `seek(Start(0))` 重读。
pub fn seek(fd: usize, pos: SeekFrom) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    file.offset = f.seek(pos, file.offset)?;
    Ok(file.offset)
}

/// 发送设备控制命令。
#[allow(dead_code)] // ioctl 语义预留
pub fn control(fd: usize, cmd: u32, arg: usize) -> Result<isize> {
    let table = FILE_TABLE.read();
    let file = table
        .files
        .get(fd)
        .and_then(|f| f.as_ref())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    f.control(cmd, arg)
}
