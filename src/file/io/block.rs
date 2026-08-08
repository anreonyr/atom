// file/io/block — 块设备服务集成（设备 → File 接缝的块版本）
//
// 仿 file/io/console（终端核心）的接缝模板：hal 能力契约（BlockDevice）→
// 非泛型 register → File 视图 + 中断 handler 归设备。与终端的差异：
//   - BlockFile 用 offset 做**随机块访问**（File offset 参数的主战场——块设备
//     正是 VFS 把 OpenFile::offset 传入 read/write 的典型消费方），Console 忽略
//     offset（流式）。
//   - 中断语义是"请求完成"而非"数据到达"：BlockIrqHandler 只 ack + 唤醒等待者。
//
// register(device: &'static dyn BlockDevice) 非泛型——新块设备（virtio-blk 等）
// 只需实现 BlockDevice 能力即可复用整套块服务（/dev/block0 + 中断唤醒）。

use alloc::boxed::Box;
use alloc::vec;

use crate::file::ops::{File, FileError, Result, SeekFrom};
use crate::file::registry;
use crate::hal::InterruptHandler;
use crate::hal::block::BlockDevice;

/// 块设备 File 视图 — `/dev/block0`（随机访问，offset 定位块）。
pub struct BlockFile {
    device: &'static dyn BlockDevice,
}

impl File for BlockFile {
    /// 从 `offset` 读最多 `buf.len()` 字节（随机块访问；到盘尾截断，返回实读字节数）。
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
        let bs = self.device.block_size();
        let total = self.device.block_count() * bs;
        if offset >= total || buf.is_empty() {
            return Ok(0);
        }
        let end = total.min(offset + buf.len());
        let mut staging = vec![0u8; bs];
        let mut n = 0;
        while offset + n < end {
            let pos = offset + n;
            let block = (pos / bs) as u32;
            let off = pos % bs;
            self.device.read_block(block, &mut staging);
            let take = (bs - off).min(end - (offset + n));
            buf[n..n + take].copy_from_slice(&staging[off..off + take]);
            n += take;
        }
        Ok(n)
    }

    /// 从 `offset` 写入 `buf`（随机块访问；到盘尾截断，返回实写字节数）。
    ///
    /// 首尾部分块读-改-写（整块读 → 覆盖 → 整块写回），保证不毁相邻数据。
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize> {
        let bs = self.device.block_size();
        let total = self.device.block_count() * bs;
        if offset >= total || buf.is_empty() {
            return Ok(0);
        }
        let end = total.min(offset + buf.len());
        let mut staging = vec![0u8; bs];
        let mut n = 0;
        while offset + n < end {
            let pos = offset + n;
            let block = (pos / bs) as u32;
            let off = pos % bs;
            let chunk = (bs - off).min(end - (offset + n));
            self.device.read_block(block, &mut staging); // 整块读（RMW 前提）
            staging[off..off + chunk].copy_from_slice(&buf[n..n + chunk]);
            self.device.write_block(block, &staging); // 整块写回
            n += chunk;
        }
        Ok(n)
    }

    /// 块设备大小 = 整盘字节数（fstat 元数据；普通文件的 size 语义在 file::fs）。
    fn size(&self) -> usize {
        self.device.block_count() * self.device.block_size()
    }

    /// 支持 `End` 定位（块设备可 seek 到盘尾）。
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

/// 块设备完成中断处理器 — 归设备，不归 BlockFile。
///
/// virtio 完成中断 → ACK 设备中断位（防 PLIC 挂起）+ 唤醒块事件等待者
/// （`schedule::signal_event(Event::Block)`；锁内提交走轮询分支时无等待者，空 ack
/// 无害）。
pub struct BlockIrqHandler {
    device: &'static dyn BlockDevice,
}

impl InterruptHandler for BlockIrqHandler {
    fn interrupt_number(&self) -> u32 {
        self.device.interrupt_number()
    }

    fn handle_interrupt(&self) {
        self.device.ack_interrupt();
        crate::schedule::signal_event(crate::schedule::Event::Block);
    }
}

/// 注册一个块设备（驱动 probe 内调用，Linux 注册块设备的对应物）。
///
/// 非泛型入口——设备无关：三联动注册
///   1. hal 单例（`hal::block::register`，file::fs 挂载消费）
///   2. 复用器注册表：`/dev/block0`（BlockFile）
///   3. trap 中断处理器（BlockIrqHandler：完成中断 → ack + 唤醒）
///
/// devfs 构建时枚举 registry 自动收录 `/dev/block0`（同 consoleN 路径）。
pub fn register(device: &'static dyn BlockDevice) {
    crate::hal::block::register(device);

    let file: &'static BlockFile = Box::leak(Box::new(BlockFile { device }));
    let _ = registry::register("/dev/block0", file);

    let handler: &'static dyn InterruptHandler = Box::leak(Box::new(BlockIrqHandler { device }));
    crate::trap::register_interrupt_handler(handler);
}
