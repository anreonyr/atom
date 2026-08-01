// /dev/zero — 零字节源
//
// 读取返回零填充缓冲区，写入被丢弃。

use crate::file::{File, Result};

/// 零设备 — 读取返回零字节。
pub struct ZeroDev;

/// 全局零设备实例。
pub static ZERO: ZeroDev = ZeroDev;

// SAFETY: ZeroDev 是 ZST，无内部状态。
unsafe impl Send for ZeroDev {}
unsafe impl Sync for ZeroDev {}

impl File for ZeroDev {
    /// 用零填充缓冲区，返回填充的字节数。
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    /// 吞掉所有数据，返回写入字节数。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        Ok(buf.len())
    }
}
