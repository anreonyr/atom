// file/fs/dir — 目录动态能力（FsDir：磁盘懒物化子 Inode）
//
// FsDir 实现 `Directory`：目录子节点在磁盘上（数据块内目录项），运行时经
// lookup_child/create_child 懒物化 `&'static Inode`（Box::leak）并缓存到
// `children`。所有磁盘操作持 `SpinLock<DiskInode>`（SIE=0 → 块 I/O 走轮询，
// 从设计上杜绝"持锁 wfi"）。

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::file::fs::{
    append_dir_entry, find_dirent, for_each_dirent, materialize, read_inode, write_inode,
    TY_DIR, TY_FILE, MAX_NAME,
};
use crate::file::ops::{FileError, Result};
use crate::file::vfs::inode::{Directory, Inode, InodeType};
use crate::hal::block::BlockDevice;
use crate::lock::SpinLock;

/// 目录动态能力 — 磁盘目录数据 + 已物化子 Inode 缓存。
pub(crate) struct FsDir {
    device: &'static dyn BlockDevice,
    ino: u32,
    /// 本目录 on-disk inode 缓存（size + direct 块指针）
    disk_inode: SpinLock<crate::file::fs::DiskInode>,
    /// 已物化子 Inode 缓存（(名称, Inode)；名称 = inode.name，静态）
    children: SpinLock<Vec<(&'static str, &'static Inode)>>,
}

impl FsDir {
    /// 构造目录能力（读 on-disk inode 入缓存），`Box::leak` 得 `'static`。
    pub(crate) fn new(device: &'static dyn BlockDevice, ino: u32) -> &'static Self {
        let di = read_inode(device, ino);
        Box::leak(Box::new(Self {
            device,
            ino,
            disk_inode: SpinLock::new(di),
            children: SpinLock::new(Vec::new()),
        }))
    }
}

impl Directory for FsDir {
    /// 按名解析子节点：缓存命中 → 磁盘目录项 → 懒物化 Inode 入缓存。
    fn lookup_child(&self, name: &str) -> Option<&'static Inode> {
        // 缓存命中
        {
            let cache = self.children.lock();
            if let Some((_, inode)) = cache.iter().find(|(n, _)| *n == name) {
                return Some(inode);
            }
        }
        // 磁盘扫描（持 disk_inode 锁，SIE=0 → 块 I/O 轮询）
        let ino = {
            let di = self.disk_inode.lock();
            find_dirent(self.device, &di, name)
        }?;
        // 物化 + 入缓存（锁外，单 hart 下重复物化窗口极小，接受）
        let inode = materialize(self.device, ino, name);
        self.children.lock().push((inode.name, inode));
        Some(inode)
    }

    /// 创建子节点：分配 inode + 追加目录项（写盘）+ 物化入缓存。
    ///
    /// 已存在（磁盘或缓存）→ 返回现有；名字非法/磁盘满 → `FileError`。
    fn create_child(&self, name: &str, ty: InodeType) -> Result<&'static Inode> {
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(FileError::InvalidArg);
        }
        // 已存在 → 返回现有（O_CREAT 语义，不 O_EXCL）
        if let Some(existing) = self.lookup_child(name) {
            return Ok(existing);
        }
        let disk_ty = match ty {
            InodeType::Directory => TY_DIR,
            _ => TY_FILE,
        };
        // 分配 inode + 追加目录项（写盘），再写回本目录 inode
        let ino = crate::file::fs::alloc_inode(self.device, disk_ty)?;
        {
            let mut di = self.disk_inode.lock();
            append_dir_entry(self.device, &mut di, ino, name)?;
            write_inode(self.device, self.ino, &di);
        }
        let inode = materialize(self.device, ino, name);
        self.children.lock().push((inode.name, inode));
        Ok(inode)
    }

    /// 一次性列举子节点名（"name\n"），buf 满整项截断。
    fn readdir(&self, buf: &mut [u8]) -> Result<usize> {
        let di = self.disk_inode.lock();
        let mut written = 0;
        for_each_dirent(self.device, &di, |entry| {
            let name = entry.name().as_bytes();
            let need = name.len() + 1;
            if written + need <= buf.len() {
                buf[written..written + name.len()].copy_from_slice(name);
                buf[written + name.len()] = b'\n';
                written += need;
            }
        });
        Ok(written)
    }
}
