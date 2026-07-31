// 文件描述符表 — 打开文件管理
//
// FileTable 维护 fd (小整数) → OpenFile 的映射。
// 当前为全局表（内核单地址空间），后续多进程时每个进程持有一个 FileTable。

use alloc::vec::Vec;

use crate::filesystem::inode::{resolve, Inode};
use crate::filesystem::traits::{Error, OpenFlags, Result, SeekFrom};
use crate::lock::{OnceLock, RwLock};

// ── OpenFile ──────────────────────────────────────────────

/// 打开的文件描述 — 携带 per-open 状态。
pub struct OpenFile {
    /// 指向文件系统节点的引用
    pub inode: &'static Inode,
    /// 当前文件偏移（字节设备忽略）
    pub offset: usize,
    /// 打开标志
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

/// 全局文件描述符表 — RwLock 保护（读多写少，参考 hub.rs TABLE）。
static FILE_TABLE: RwLock<FileTable> = RwLock::new(FileTable::new());

/// 根 Inode — OnceLock 写一次读多次（参考 tree.rs DEVICES）。
static ROOT_INODE: OnceLock<&'static Inode> = OnceLock::new();

/// 初始化命名空间根节点（引导期调用一次）。
pub fn set_root(root: &'static Inode) {
    ROOT_INODE
        .set(root)
        .expect("filesystem: root already initialized");
}

// ── 公共 API ──────────────────────────────────────────────

/// 打开指定路径的文件，返回文件描述符。
///
/// fd 分配策略：扫描已关闭的 slot 复用；无空闲时追加到末尾。
pub fn open(path: &str, flags: OpenFlags) -> Result<usize> {
    let root = ROOT_INODE.get().ok_or(Error::NotFound)?;
    let inode = resolve(root, path).ok_or(Error::NotFound)?;

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
        return Err(Error::InvalidFd);
    }
    table.files[fd] = None;
    Ok(())
}

/// 从文件描述符读取数据。
///
/// 若 inode 支持 seek，读取前先 seek 到当前偏移；读后推进偏移。
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(Error::InvalidFd)?;

    let reader = file.inode.read.ok_or(Error::NotSupported)?;

    // 若文件支持 seek，先定位到当前偏移
    if let Some(seeker) = file.inode.seek {
        seeker.seek(SeekFrom::Start(file.offset))?;
    }

    let n = reader.read(buf)?;
    file.offset += n;
    Ok(n)
}

/// 向文件描述符写入数据。
///
/// 若 inode 支持 seek，写入前先 seek 到当前偏移；写后推进偏移。
pub fn write(fd: usize, buf: &[u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(Error::InvalidFd)?;

    let writer = file.inode.write.ok_or(Error::NotSupported)?;

    // 若文件支持 seek，先定位到当前偏移
    if let Some(seeker) = file.inode.seek {
        seeker.seek(SeekFrom::Start(file.offset))?;
    }

    let n = writer.write(buf)?;
    file.offset += n;
    Ok(n)
}

/// 设置文件描述符偏移。
///
/// 对于字节设备（无 seek），偏移仅在 OpenFile 中记录，不影响实际 I/O。
pub fn lseek(fd: usize, pos: SeekFrom) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(Error::InvalidFd)?;

    match pos {
        SeekFrom::Start(off) => file.offset = off,
        SeekFrom::Current(delta) => {
            let new = (file.offset as isize).wrapping_add(delta);
            if new < 0 {
                return Err(Error::InvalidArg);
            }
            file.offset = new as usize;
        }
        SeekFrom::End(delta) => {
            // 委托 inode.seek 获取文件末尾位置
            let seeker = file.inode.seek.ok_or(Error::NotSupported)?;
            let end = seeker.seek(SeekFrom::End(0))?;
            let new = (end as isize).wrapping_add(delta);
            if new < 0 {
                return Err(Error::InvalidArg);
            }
            file.offset = new as usize;
        }
    }
    Ok(file.offset)
}

/// 发送设备控制命令。
pub fn control(fd: usize, cmd: u32, arg: usize) -> Result<isize> {
    let table = FILE_TABLE.read();
    let file = table
        .files
        .get(fd)
        .and_then(|f| f.as_ref())
        .ok_or(Error::InvalidFd)?;

    let ctrl = file.inode.control.ok_or(Error::NotSupported)?;
    ctrl.control(cmd, arg)
}
