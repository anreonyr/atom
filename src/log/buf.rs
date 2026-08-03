// 定长栈缓冲 — 兼作 fmt::Write 目标
//
// 日志格式化全程无堆：Buf 是栈上定长数组 + 长度，容量不足静默截断。
// 服务对象：log_line（消息体/定位段）、fmt_time（时间戳）、log_read（行组装）。

use core::fmt;

/// 定长栈缓冲 — 兼作 fmt::Write 目标，把日志格式化进无堆缓冲。
#[derive(Clone, Copy)]
pub(super) struct Buf<const N: usize> {
    data: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    pub(super) const fn new() -> Self {
        Buf {
            data: [0; N],
            len: 0,
        }
    }

    pub(super) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.data[..self.len]).unwrap_or("")
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // 容量不足时静默截断
        let room = N - self.len;
        let n = s.len().min(room);
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}
