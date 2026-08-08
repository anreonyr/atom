// /dev/null — 数据黑洞
//
// 所有写入被丢弃，读取始终返回 EOF。

use crate::file::ops::{File, Result};

/// 空设备 — 数据黑洞。
pub struct NullDev;

/// 全局空设备实例。
pub static NULL: NullDev = NullDev;

// SAFETY: NullDev 是 ZST，无内部状态。
unsafe impl Send for NullDev {}
unsafe impl Sync for NullDev {}

impl File for NullDev {
    /// 始终返回 EOF（读取 0 字节）。
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    /// 吞掉所有数据，返回写入字节数。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        Ok(buf.len())
    }
}
