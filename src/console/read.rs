// 输入通道 — 读取 console 输入（对应 print.rs 的输出通道）
//
// print.rs 输出：write/write_to → device::preferred_writer()（preferred 输出设备）
// read.rs 输入：read/read_byte/read_to → device::preferred_buffer()（preferred 输入设备）
//
// 语义（对应 Linux tty_read 的阻塞读）：
//   - read(buf)    阻塞读取，尽量填满 buf，返回实际读取字节数（≥1）
//   - read_byte()  阻塞读取单字节
//   - read_to()    阻塞读取到裸指针（envcall 写用户缓冲区用）
// 无输入设备 → Err(NotFound)：输入侧无兜底设备，避免永久阻塞，调用方降级。

use crate::device;
use crate::filesystem::ops::{File, FileError, Result}; // 直连契约叶子：filesystem::dev → console::read 反向依赖，不走 filesystem 重导出防环

/// console 标准流设备 — 唯一的设备 File 视图（stdin/stdout 统一）。
///
/// read 从 device 层 preferred 输入源取（**非阻塞单次尝试**：缓冲空返回
/// [`FileError::WouldBlock`]，由调用方——envcall syscall 层——决定等待方式：
/// 置 WaitRead → Reschedule park，唤醒后重放分发）；write 经 device 层
/// preferred 输出（sink writer 的 `write_str` 已做 CRLF，与 println! 一致）。
/// 字节设备无 EOF。devfs 的 /dev/stdin 与 /dev/stdout 均挂本类型。
pub struct Console;

impl File for Console {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        // SAFETY: File::read 的 buf 是 &mut [u8]，天然保证可写 buf.len() 字节
        // （read_to 的裸指针约束由 slice 所有权满足）。
        unsafe { read_to(buf.as_mut_ptr(), buf.len()) }
    }

    /// 向 preferred 输出设备写入（文本视图：非 UTF-8 字节丢弃，教学取舍，
    /// 与 print!/println! 同路径——console 设备层是 fmt::Write）。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        let w = crate::device::preferred_writer();
        let s = core::str::from_utf8(buf).unwrap_or("");
        let _ = core::fmt::Write::write_str(w, s);
        Ok(buf.len())
    }
}

/// 等待输入可用：缓冲空时阻塞（任务 park 等字符到达，见
/// [`crate::schedule::input_wait`]——SMode 恢复点 / UMode 重放 ecall）。
fn wait_input() -> Result<()> {
    crate::schedule::input_wait();
    Ok(())
}

/// 阻塞取一字节：从 `buffer` pop，空则等待输入事件。
fn pop_blocking(buffer: &'static device::InputBuffer) -> Result<u8> {
    loop {
        if let Some(c) = buffer.pop() {
            return Ok(c);
        }
        wait_input()?;
    }
}

/// 从 preferred 输入设备阻塞读取，尽量填满 `buf`，返回实际读取字节数（≥1）。
///
/// 无输入设备 → [`FileError::NotFound`]（输入侧无 SBI 兜底，调用方自行降级）。
pub fn read(buf: &mut [u8]) -> Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let buffer = device::preferred_buffer().ok_or(FileError::NotFound)?;
    buf[0] = pop_blocking(buffer)?;
    let mut n = 1;
    while n < buf.len() {
        let Some(c) = buffer.pop() else { break };
        buf[n] = c;
        n += 1;
    }
    Ok(n)
}

/// 从 preferred 输入设备阻塞读取单字节。
pub fn read_byte() -> Result<u8> {
    let mut b = [0u8; 1];
    read(&mut b)?;
    Ok(b[0])
}

/// 尝试读取到裸指针（envcall 写用户缓冲区用），尽量填满，返回读取字节数。
///
/// **非阻塞单次尝试**：缓冲空返回 [`FileError::WouldBlock`]——envcall 在
/// trap 上下文（SIE=0）不能 wfi，由调用方（dispatch）决定等待方式（置
/// WaitRead → trap_handler 重放 ecall）；缓冲有数据则尽量填（≥1 字节）。
///
/// # Safety
///
/// 调用方（envcall 分发）必须保证 `buf` 指向至少 `count` 字节的可写内存——
/// U 任务传入的用户缓冲区（SUM 已置位，S-mode 可写 U 页）。信任用户指针，
/// 不做映射校验（真实内核需 copy_to_user，骨架阶段从简）。
pub unsafe fn read_to(buf: *mut u8, count: usize) -> Result<usize> {
    if count == 0 {
        return Ok(0);
    }
    let buffer = device::preferred_buffer().ok_or(FileError::NotFound)?;
    let mut n = 0;
    while n < count {
        let Some(c) = buffer.pop() else { break };
        // SAFETY: 由调用方保证 buf 可写 count 字节（n < count）。
        unsafe { buf.add(n).write_volatile(c) };
        n += 1;
    }
    if n == 0 {
        return Err(FileError::WouldBlock);
    }
    Ok(n)
}
