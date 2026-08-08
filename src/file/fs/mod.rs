// file/fs — 自制极简文件系统（块设备之上，动态目录）
//
// 职责：在 `hal::BlockDevice` 之上实现一个可持久化的极简 FS——superblock +
// inode 表 + 数据位图 + 数据区 + 目录。`mount` 读 superblock（magic 非法则
// format），返回 `/data` 挂载点的 Inode（dir = FsDir 动态目录能力）；文件/
// 子目录经 `Directory::lookup_child`/`create_child` 运行时**懒物化**。
//
// 持久性：所有写（数据块 / inode 表 / 位图）都 write-through 到块设备——
// 无需 fsync；重启后 mount 从磁盘读回。验收 = U 程序写文件 → 重启 → 读回一致。
//
// on-disk 布局（FS 块 = 设备块 = 512B）：
//   block 0             superblock { magic="ATOM", inode_count, inode_table,
//                                     bitmap, data_start, block_count }
//   inode_table ..       inode 表（DiskInode 64B → 8 个/块；ino 0 无效，root=ino 1）
//   bitmap ..           数据位图（1 bit/数据块）
//   data_start ..       数据区
//
// 依赖方向：file::fs → hal::block + file::vfs（Inode/Directory）+ file::ops +
// lock。不依赖 driver（只消费 `hal::BlockDevice` 能力契约）。

mod dir;
mod file;

use alloc::boxed::Box;
use alloc::string::ToString;

use crate::file::ops::{FileError, Result};
use crate::file::vfs::inode::{Inode, InodeBuilder, InodeType};
use crate::hal::block::BlockDevice;
use crate::lock::OnceLock;

// FsDir/FsFile 仅在模块内部使用（materialize）。
use dir::FsDir;
use file::FsFile;

// ── 常量 ─────────────────────────────────────────────────

/// FS 块大小（字节）— 等于设备块大小（virtio-blk 512）。
pub(crate) const BLOCK_SIZE: usize = 512;
/// 文件名最大长度（不含 NUL）。
pub(crate) const MAX_NAME: usize = 28;
/// 每个 inode 的直接块指针数（文件最大 12×512 = 6 KB，探针足够）。
pub(crate) const DIRECT: usize = 12;
/// inode 总数（ino 1..=INODE_COUNT）。
const INODE_COUNT: u32 = 64;
/// 每块 inode 数（DiskInode 64B）。
const INODES_PER_BLOCK: u32 = 8;
/// 每块目录项数（DirEntry 32B）。
const DIRENT_PER_BLOCK: usize = 16;

/// on-disk inode 类型。
const TY_FREE: u32 = 0;
pub(crate) const TY_FILE: u32 = 1;
pub(crate) const TY_DIR: u32 = 2;

// ── on-disk 结构（repr(C)，直接读写块缓冲）──────────────────

/// superblock — 块 0。
#[derive(Clone, Copy)]
#[repr(C)]
struct SuperBlock {
    magic: [u8; 4], // "ATOM"
    inode_count: u32,
    inode_table: u32,
    bitmap: u32,
    data_start: u32,
    block_count: u32,
}

/// on-disk inode — 64B（8 个/块）。
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct DiskInode {
    pub(crate) ty: u32,
    pub(crate) size: u32,
    pub(crate) direct: [u32; DIRECT],
    reserved: [u32; 2], // 补齐 64B
}

/// on-disk 目录项 — 32B（16 个/块；ino==0 空槽，name NUL 结尾）。
#[derive(Clone, Copy)]
#[repr(C)]
struct DirEntry {
    ino: u32,
    name: [u8; MAX_NAME],
}

impl DirEntry {
    /// 名称（NUL 截断）。
    fn name(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(MAX_NAME);
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }
}

/// 挂载后的 superblock 缓存（低层辅助读取布局用）。
static SUPER: OnceLock<SuperBlock> = OnceLock::new();

// ── 低层块辅助 ────────────────────────────────────────────

/// 读一整块到 `buf`。
pub(crate) fn rb(dev: &dyn BlockDevice, block: u32, buf: &mut [u8]) {
    dev.read_block(block, buf);
}

/// 写一整块自 `buf`。
pub(crate) fn wb(dev: &dyn BlockDevice, block: u32, buf: &[u8]) {
    dev.write_block(block, buf);
}

/// 读 superblock（magic 非法返回 None）。
fn read_super(dev: &dyn BlockDevice) -> Option<SuperBlock> {
    let mut buf = [0u8; BLOCK_SIZE];
    rb(dev, 0, &mut buf);
    // SAFETY: buf 512B ≥ SuperBlock 24B；repr(C) 布局无 UB（read_unaligned）。
    let sb = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const SuperBlock) };
    if &sb.magic != b"ATOM" {
        return None;
    }
    Some(sb)
}

/// 写 superblock。
fn write_super(dev: &dyn BlockDevice, sb: &SuperBlock) {
    let mut buf = [0u8; BLOCK_SIZE];
    // SAFETY: SuperBlock 24B ≤ buf；repr(C)。
    unsafe {
        core::ptr::copy_nonoverlapping(
            sb as *const SuperBlock as *const u8,
            buf.as_mut_ptr(),
            core::mem::size_of::<SuperBlock>(),
        );
    }
    wb(dev, 0, &buf);
}

/// 读 on-disk inode（ino 1-based）。
pub(crate) fn read_inode(dev: &dyn BlockDevice, ino: u32) -> DiskInode {
    let sb = *SUPER.get().expect("fs not mounted");
    let block = sb.inode_table + (ino - 1) / INODES_PER_BLOCK;
    let slot = ((ino - 1) % INODES_PER_BLOCK) as usize;
    let mut buf = [0u8; BLOCK_SIZE];
    rb(dev, block, &mut buf);
    let off = slot * core::mem::size_of::<DiskInode>();
    // SAFETY: slot < 8，off+64 ≤ 512；repr(C)。
    unsafe { core::ptr::read_unaligned(buf.as_ptr().add(off) as *const DiskInode) }
}

/// 写 on-disk inode（保留同块其它 inode）。
pub(crate) fn write_inode(dev: &dyn BlockDevice, ino: u32, di: &DiskInode) {
    let sb = *SUPER.get().expect("fs not mounted");
    let block = sb.inode_table + (ino - 1) / INODES_PER_BLOCK;
    let slot = ((ino - 1) % INODES_PER_BLOCK) as usize;
    let mut buf = [0u8; BLOCK_SIZE];
    rb(dev, block, &mut buf); // 读原块保留其它 inode
    let off = slot * core::mem::size_of::<DiskInode>();
    // SAFETY: slot < 8，off+64 ≤ 512；repr(C)。
    unsafe {
        core::ptr::copy_nonoverlapping(
            di as *const DiskInode as *const u8,
            buf.as_mut_ptr().add(off),
            core::mem::size_of::<DiskInode>(),
        );
    }
    wb(dev, block, &buf);
}

/// 解析目录项缓冲（32B）。
fn read_dir_entry(buf: &[u8]) -> DirEntry {
    let mut name = [0u8; MAX_NAME];
    name.copy_from_slice(&buf[4..4 + MAX_NAME]);
    DirEntry {
        ino: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
        name,
    }
}

/// 序列化目录项缓冲（32B）。
fn write_dir_entry(buf: &mut [u8], ino: u32, name: &str) {
    buf[0..4].copy_from_slice(&ino.to_le_bytes());
    let n = name.len().min(MAX_NAME);
    buf[4..4 + n].copy_from_slice(&name.as_bytes()[..n]);
    buf[4 + n..4 + MAX_NAME].fill(0);
}

/// 分配一个数据块（扫位图置 1），返回绝对块号。
pub(crate) fn alloc_block(dev: &dyn BlockDevice) -> Result<u32> {
    let sb = *SUPER.get().expect("fs not mounted");
    let data_blocks = sb.block_count - sb.data_start;
    let mut buf = [0u8; BLOCK_SIZE];
    for b in 0..data_blocks {
        let bitmap_block = sb.bitmap + b / (BLOCK_SIZE as u32 * 8);
        if b % (BLOCK_SIZE as u32 * 8) == 0 {
            rb(dev, bitmap_block, &mut buf);
        }
        let byte = (b % (BLOCK_SIZE as u32 * 8)) as usize / 8;
        let bit = 1 << (b % 8);
        if buf[byte] & bit == 0 {
            buf[byte] |= bit;
            wb(dev, bitmap_block, &buf);
            return Ok(sb.data_start + b);
        }
    }
    Err(FileError::InvalidArg) // 磁盘满
}

/// 分配一个 inode 槽（扫 inode 表找 TY_FREE），写初值返回 ino。
pub(crate) fn alloc_inode(dev: &dyn BlockDevice, ty: u32) -> Result<u32> {
    let sb = *SUPER.get().expect("fs not mounted");
    for ino in 1..=sb.inode_count {
        let mut di = read_inode(dev, ino);
        if di.ty == TY_FREE {
            di.ty = ty;
            di.size = 0;
            di.direct = [0; DIRECT];
            write_inode(dev, ino, &di);
            return Ok(ino);
        }
    }
    Err(FileError::InvalidArg) // inode 表满
}

/// 在目录 inode 的数据块中按名查目录项，返回子 ino。
pub(crate) fn find_dirent(dev: &dyn BlockDevice, di: &DiskInode, name: &str) -> Option<u32> {
    let blocks = (di.size as usize).div_ceil(BLOCK_SIZE);
    for i in 0..blocks.min(DIRECT) {
        let block = di.direct[i];
        if block == 0 {
            break;
        }
        let mut buf = [0u8; BLOCK_SIZE];
        rb(dev, block, &mut buf);
        for slot in 0..DIRENT_PER_BLOCK {
            let e = read_dir_entry(&buf[slot * 32..slot * 32 + 32]);
            if e.ino != 0 && e.name() == name {
                return Some(e.ino);
            }
        }
    }
    None
}

/// 遍历目录 inode 数据块中的全部非空目录项。
fn for_each_dirent(dev: &dyn BlockDevice, di: &DiskInode, mut f: impl FnMut(DirEntry)) {
    let blocks = (di.size as usize).div_ceil(BLOCK_SIZE);
    for i in 0..blocks.min(DIRECT) {
        let block = di.direct[i];
        if block == 0 {
            break;
        }
        let mut buf = [0u8; BLOCK_SIZE];
        rb(dev, block, &mut buf);
        for slot in 0..DIRENT_PER_BLOCK {
            let e = read_dir_entry(&buf[slot * 32..slot * 32 + 32]);
            if e.ino != 0 {
                f(e);
            }
        }
    }
}

/// 在目录 inode 中追加一个目录项（找空槽，无则分配新数据块），更新 size。
pub(crate) fn append_dir_entry(
    dev: &dyn BlockDevice,
    di: &mut DiskInode,
    ino: u32,
    name: &str,
) -> Result<()> {
    let existing = (di.size as usize).div_ceil(BLOCK_SIZE);
    // 现有块中找空槽
    for i in 0..existing.min(DIRECT) {
        let block = di.direct[i];
        if block == 0 {
            break;
        }
        let mut buf = [0u8; BLOCK_SIZE];
        rb(dev, block, &mut buf);
        for slot in 0..DIRENT_PER_BLOCK {
            let off = slot * 32;
            if read_dir_entry(&buf[off..off + 32]).ino == 0 {
                write_dir_entry(&mut buf[off..off + 32], ino, name);
                wb(dev, block, &buf);
                di.size = di
                    .size
                    .max((i as u32) * BLOCK_SIZE as u32 + (slot as u32 + 1) * 32);
                return Ok(());
            }
        }
    }
    // 无空槽 → 分配新数据块
    if existing >= DIRECT {
        return Err(FileError::InvalidArg); // 目录过大
    }
    let new_block = alloc_block(dev)?;
    di.direct[existing] = new_block;
    let mut buf = [0u8; BLOCK_SIZE];
    write_dir_entry(&mut buf[0..32], ino, name);
    wb(dev, new_block, &buf);
    di.size = ((existing + 1) as u32) * BLOCK_SIZE as u32;
    Ok(())
}

// ── 挂载 / 格式化 ─────────────────────────────────────────

/// 挂载块设备为 `/data` 子树：读 superblock，magic 非法则格式化。
///
/// 返回挂载点 Inode（name="data"，dir = FsDir 动态目录）。无有效设备/格式化
/// 失败返回 None（init.rs 据此不加 /data 子树，系统无 FS 仍可运行）。
pub fn mount(device: &'static dyn BlockDevice) -> Option<&'static Inode> {
    let sb = match read_super(device) {
        Some(s) if &s.magic == b"ATOM" => s,
        _ => format(device),
    };
    let _ = SUPER.set(sb);

    let root = FsDir::new(device, 1); // root 目录 ino 恒 1
    Some(
        InodeBuilder::new("data", InodeType::Directory)
            .with_dir(root)
            .build(),
    )
}

/// 格式化：写 superblock + 清零 inode 表/位图 + 建 root 目录（ino 1）。
fn format(dev: &dyn BlockDevice) -> SuperBlock {
    let block_count = dev.block_count() as u32;
    let inode_table = 1u32;
    let inode_table_blocks = INODE_COUNT.div_ceil(INODES_PER_BLOCK); // 8
    let bitmap = inode_table + inode_table_blocks;
    let data_blocks = block_count - bitmap; // super + inode 表 + 位图占前部
    let bitmap_blocks = data_blocks.div_ceil(BLOCK_SIZE as u32 * 8);
    let data_start = bitmap + bitmap_blocks;

    let sb = SuperBlock {
        magic: *b"ATOM",
        inode_count: INODE_COUNT,
        inode_table,
        bitmap,
        data_start,
        block_count,
    };
    let _ = SUPER.set(sb); // write_inode 需要 SUPER
    write_super(dev, &sb);

    let zero = [0u8; BLOCK_SIZE];
    for b in inode_table..data_start {
        wb(dev, b, &zero);
    }
    let root = DiskInode {
        ty: TY_DIR,
        size: 0,
        direct: [0; DIRECT],
        reserved: [0; 2],
    };
    write_inode(dev, 1, &root);
    sb
}

/// 物化一个磁盘 inode 为 `&'static Inode`（按 ty 建 FsFile/FsDir + Box::leak）。
pub(crate) fn materialize(
    device: &'static dyn BlockDevice,
    ino: u32,
    name: &str,
) -> &'static Inode {
    let di = read_inode(device, ino);
    let name: &'static str = Box::leak(name.to_string().into_boxed_str());
    match di.ty {
        TY_DIR => InodeBuilder::new(name, InodeType::Directory)
            .with_dir(FsDir::new(device, ino))
            .build(),
        _ => InodeBuilder::new(name, InodeType::File)
            .with_file(FsFile::new(device, ino))
            .build(),
    }
}
