// file/fs/file — 文件 File 视图（FsFile：磁盘数据随机访问）
//
// FsFile 实现 `File`：read/write 按 offset 定位到 on-disk 数据块（direct 指针），
// 写超 size 追加分配数据块并写回 inode（size/direct 持久化）。内部 `SpinLock
// <DiskInode>` 缓存 inode（size + direct），所有磁盘操作持锁（SIE=0 → 块 I/O
// 轮询，杜绝"持锁 wfi"）。

use alloc::boxed::Box;

use crate::file::fs::{alloc_block, read_inode, write_inode, BLOCK_SIZE, DIRECT};
use crate::file::ops::{File, FileError, Result, SeekFrom};
use crate::hal::block::BlockDevice;
use crate::lock::SpinLock;

/// 文件 File 视图 — on-disk inode 缓存 + 块设备随机访问。
pub(crate) struct FsFile {
    device: &'static dyn BlockDevice,
    ino: u32,
    disk_inode: SpinLock<crate::file::fs::DiskInode>,
}

impl FsFile {
    /// 构造文件视图（读 on-disk inode 入缓存），`Box::leak` 得 `'static`。
    pub(crate) fn new(device: &'static dyn BlockDevice, ino: u32) -> &'static Self {
        let di = read_inode(device, ino);
        Box::leak(Box::new(Self {
            device,
            ino,
            disk_inode: SpinLock::new(di),
        }))
    }
}

impl File for FsFile {
    /// 从 `offset` 读最多 `buf.len()` 字节（到文件尾截断，返回实读字节数）。
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
        let di = self.disk_inode.lock();
        let size = di.size as usize;
        if offset >= size || buf.is_empty() {
            return Ok(0); // EOF
        }
        let end = size.min(offset + buf.len());
        let mut n = 0;
        while offset + n < end {
            let pos = offset + n;
            let block = di.direct[pos / BLOCK_SIZE]; // pos < size ≤ 12×512 → in-range
            let off = pos % BLOCK_SIZE;
            let mut block_buf = [0u8; BLOCK_SIZE];
            self.device.read_block(block, &mut block_buf);
            let take = (BLOCK_SIZE - off).min(end - (offset + n));
            buf[n..n + take].copy_from_slice(&block_buf[off..off + take]);
            n += take;
        }
        Ok(n)
    }

    /// 从 `offset` 写 `buf`（超 size 追加分配数据块 + 写回 inode）。
    ///
    /// 部分块读-改-写（RMW：新分配块读回为 0，整块覆盖写回防毁相邻数据）。
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize> {
        let mut di = self.disk_inode.lock();
        let end = offset.saturating_add(buf.len());
        let blocks_needed = end.div_ceil(BLOCK_SIZE);
        if blocks_needed > DIRECT {
            return Err(FileError::InvalidArg); // 文件超过 12 块（6 KB）上限
        }
        // 追加分配缺失的块（直接指针）
        let mut current = (di.size as usize).div_ceil(BLOCK_SIZE);
        while current < blocks_needed {
            let new_block = alloc_block(self.device)?;
            di.direct[current] = new_block;
            current += 1;
        }
        // 逐块写入（RMW）
        let mut n = 0;
        while n < buf.len() {
            let pos = offset + n;
            let block = di.direct[pos / BLOCK_SIZE];
            let off = pos % BLOCK_SIZE;
            let chunk = (BLOCK_SIZE - off).min(buf.len() - n);
            let mut block_buf = [0u8; BLOCK_SIZE];
            self.device.read_block(block, &mut block_buf); // RMW 前提：读整块
            block_buf[off..off + chunk].copy_from_slice(&buf[n..n + chunk]);
            self.device.write_block(block, &block_buf);
            n += chunk;
        }
        di.size = di.size.max(end as u32);
        write_inode(self.device, self.ino, &di); // 持久化 size + direct
        Ok(buf.len())
    }

    /// 文件大小（fstat 元数据）。
    fn size(&self) -> usize {
        self.disk_inode.lock().size as usize
    }

    /// 支持 `End` 定位（文件可 seek 到末尾）。
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
            SeekFrom::End(delta) => {
                let new = (self.size() as isize).wrapping_add(delta);
                if new < 0 {
                    Err(FileError::InvalidArg)
                } else {
                    Ok(new as usize)
                }
            }
        }
    }
}
