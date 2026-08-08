// 文件描述符表 — 打开文件管理
//
// FileTable 维护 fd (小整数) → OpenFile 的映射。
// 偏移由 OpenFile 持有，I/O 时作为参数传给 inode 的 File 实现
// （Linux `struct file` 持有 f_pos、调用时传给 fops 的对应物）。
// 当前为全局表（内核单地址空间），后续多进程时每个进程持有一个 FileTable。
//
// 公共 API（open/close/read/write/seek/control）为 VFS 正式面，全部已由
// envcall syscall 层消费：read/write 走 Linux 号 63/64（fd 0/1 预置 + U 任务
// I/O），open/close/seek/control 走教学自定义号 1002–1005。

use alloc::vec::Vec;

use crate::file::vfs::inode::{Inode, InodeType, lookup};
use crate::file::ops::{File, FileError, OpenFlags, Result, SeekFrom};
use crate::lock::{OnceLock, RwLock};

// ── OpenFile ──────────────────────────────────────────────

/// 打开的文件描述 — 携带 per-open 状态（对应 Linux `struct file`）。
pub struct OpenFile {
    /// 指向文件系统节点的引用
    pub inode: &'static Inode,
    /// 当前文件偏移（I/O 时传给 File 实现；字节设备忽略）
    pub offset: usize,
    /// 打开标志
    pub flags: OpenFlags,
}

/// 文件元数据（`fstat` 返回）— 内核侧结构，syscall 层序列化为 `[u64 type; u64 size]`。
pub struct Stat {
    /// 文件类型（[`InodeType`] 判别式：Directory=0 / File=1 / ByteDevice=2 / Symlink=3）
    pub file_type: InodeType,
    /// 文件大小（字节；字节设备/目录为 0）
    pub size: usize,
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
        // println!（console 正常路径）：file 域不依赖 log（复用器不反向依赖
        // 提供方），重复设置是引导期一次性错误，正常输出即可（boot 早期
        // console 决策自动回落 sbi）。
        crate::println!("filesystem: root already initialized (set_root called more than once)");
    }
}

/// 解析路径到目标 Inode 的文件能力（只读，无锁）。
///
/// 根 Inode 未设置（devfs 未构建，boot 早期）返回 `None`——print/stdin 路径
/// 据此回落 sbi 无锁直写（「表空即早期」由「根未建 / 链接不存在」等价表达）。
/// symlink 跟随在 `lookup` 内完成（/dev/console → consoleN）；`Inode` 引导期
/// 构建后不可变，路径解析零锁。
pub fn resolve(path: &str) -> Option<&'static dyn File> {
    let root = ROOT_INODE.get()?;
    lookup(root, path)?.file
}

// ── 公共 API ──────────────────────────────────────────────

/// 打开指定路径的文件，返回文件描述符。
///
/// 经 [`lookup`] 解析（静态 children + 动态目录回退）后委托 [`open_inode`]。
pub fn open(path: &str, flags: OpenFlags) -> Result<usize> {
    let root = ROOT_INODE.get().ok_or(FileError::NotFound)?;
    let inode = lookup(root, path).ok_or(FileError::NotFound)?;
    open_inode(inode, flags)
}

/// 打开一个已解析的 Inode，返回文件描述符（`create` syscall 复用）。
///
/// fd 分配策略：扫描已关闭的 slot 复用；无空闲时追加到末尾。
pub fn open_inode(inode: &'static Inode, flags: OpenFlags) -> Result<usize> {
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

/// 创建文件并打开（Linux `O_CREAT` 语义，教学号段 create/1009）。
///
/// 拆父目录 + 文件名 → 在父目录的动态目录能力（`Directory::create_child`）上
/// 创建（磁盘上建 inode + 目录项 + 写盘），已存在则直接返回；随后打开返回 fd。
/// 仅动态目录（file::fs 的 `/data` 子树）支持 create；devfs 静态目录 → `NotDirectory`。
pub fn create(path: &str, flags: OpenFlags) -> Result<usize> {
    let root = ROOT_INODE.get().ok_or(FileError::NotFound)?;
    let (parent_path, name) = match path.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", path),
    };
    let parent = lookup(root, parent_path).ok_or(FileError::NotFound)?;
    let dir = parent.dir.ok_or(FileError::NotDirectory)?;
    let inode = dir.create_child(name, InodeType::File)?;
    open_inode(inode, flags)
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
/// 以当前偏移为参数调用 inode 的 File 实现；读前校验 fd 以**读方式**打开
/// （`flags.is_readable()`，否则 [`FileError::PermissionDenied`]）；读后推进
/// 偏移。访问模式强制在 VFS 层，设备实现（File::read）不检查。
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    if !file.flags.is_readable() {
        return Err(FileError::PermissionDenied); // fd 未以读方式打开（O_WRONLY）
    }
    let n = f.read(file.offset, buf)?;
    file.offset += n;
    Ok(n)
}

/// 向文件描述符写入数据。
///
/// 以当前偏移为参数调用 inode 的 File 实现；写前校验 fd 以**写方式**打开
/// （`flags.is_writable()`，否则 [`FileError::PermissionDenied`]）；写后推进
/// 偏移。访问模式强制在 VFS 层，设备实现（File::write）不检查。
pub fn write(fd: usize, buf: &[u8]) -> Result<usize> {
    let mut table = FILE_TABLE.write();
    let file = table
        .files
        .get_mut(fd)
        .and_then(|f| f.as_mut())
        .ok_or(FileError::InvalidFd)?;

    let f = file.inode.file.ok_or(FileError::NotSupported)?;
    if !file.flags.is_writable() {
        return Err(FileError::PermissionDenied); // fd 未以写方式打开（O_RDONLY）
    }
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

/// 查询 fd 指向节点的元数据（文件类型 + 大小）。
///
/// 类型取自 Inode（`inode_type`），大小委托 inode 的 File 实现（[`File::size`]，
/// 字节设备/目录默认 0）。fd 非法 → [`FileError::InvalidFd`]。
pub fn fstat(fd: usize) -> Result<Stat> {
    let table = FILE_TABLE.read();
    let file = table
        .files
        .get(fd)
        .and_then(|f| f.as_ref())
        .ok_or(FileError::InvalidFd)?;
    let size = file.inode.file.map(|f| f.size()).unwrap_or(0);
    Ok(Stat {
        file_type: file.inode.inode_type,
        size,
    })
}

/// 列举目录：把 inode 每个子节点名以 `"name\n"` 写入 buf，返回写入字节数。
///
/// 一次性列举（非 getdents 增量迭代）；buf 满**整项截断**（不写半条）。
/// 动态目录（file::fs）委托 `Directory::readdir`（磁盘列举）；静态 children
/// （devfs）遍历子节点。fd 非法 → [`FileError::InvalidFd`]；非目录 →
/// [`FileError::NotDirectory`]。
pub fn readdir(fd: usize, buf: &mut [u8]) -> Result<usize> {
    let table = FILE_TABLE.read();
    let file = table
        .files
        .get(fd)
        .and_then(|f| f.as_ref())
        .ok_or(FileError::InvalidFd)?;
    if file.inode.inode_type != InodeType::Directory {
        return Err(FileError::NotDirectory);
    }
    // 动态目录：目录项在磁盘上，经 Directory::readdir 列举
    if let Some(dir) = file.inode.dir {
        return dir.readdir(buf);
    }
    // 静态 children（devfs）
    let mut written = 0;
    for child in file.inode.children {
        let name = child.name.as_bytes();
        let need = name.len() + 1; // name + '\n'
        if written + need > buf.len() {
            break; // buf 满：整项截断
        }
        buf[written..written + name.len()].copy_from_slice(name);
        buf[written + name.len()] = b'\n';
        written += need;
    }
    Ok(written)
}
