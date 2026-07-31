// /dev/log — 内核日志环形缓冲读取
//
// 从 log 模块的 ring buffer 按快照语义读取最近日志。
// 只读字节设备：offset 从最旧条目起计字节，seek(Start) 可重读。

use crate::filesystem::traits::{File, Result};

/// 日志节点 — 读取内核日志环形缓冲。
pub struct LogDev;

/// 全局日志节点实例。
pub static LOG: LogDev = LogDev;

// SAFETY: LogDev 是 ZST，无内部状态；读取委托给 log::log_read（内部有锁保护）。
unsafe impl Send for LogDev {}
unsafe impl Sync for LogDev {}

impl File for LogDev {
    /// 从环形缓冲读取日志（快照语义，offset 从最旧条目计字节）。
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize> {
        Ok(crate::log::log_read(offset, buf))
    }
}
