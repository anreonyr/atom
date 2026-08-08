// stdio — 标准流句柄（std::io 的 stdin()/stdout() 对应物）
//
// Stdin/Stdout 双视图：
//   - `impl File`（非阻塞，`WouldBlock` 上抛）→ devfs /dev/stdin|stdout 挂载；
//     envcall 经 `filetable::read(0)/write(1)` 走此视图（VFS 强制访问模式）
//   - 固有阻塞方法（`read`/`read_byte`/`read_line`/`write`/`flush`）→ 内核
//     任务从 VFS 获取的 stdio 句柄（std::io 形态；`WouldBlock → input_wait`）
//   - `impl Read`/`Write`（ops 流契约）→ 委托固有方法，`writeln!`/`write!` 可用
//
// 读走 `device::preferred_buffer`（InputBuffer 自带锁）；写经 `print::write`
// （OUT 锁统一写者仲裁，与 println! 无双写者别名）。

use core::fmt;

use crate::file::ops::{File, FileError, Read, Result, Write};
use crate::file::vfs::filetable;
use crate::schedule;

use super::device;
use super::print;

/// 标准输入句柄 — fd 0（/dev/stdin）。
pub struct Stdin;

/// 标准输出句柄 — fd 1（/dev/stdout）。
pub struct Stdout;

/// 标准输入静态实例 — 供 devfs 挂载 /dev/stdin（file::registry 注册）。
pub static STDIN: Stdin = Stdin;

/// 标准输出静态实例 — 供 devfs 挂载 /dev/stdout（file::registry 注册）。
pub static STDOUT: Stdout = Stdout;

/// 获取标准输入句柄（std::io::stdin() 对应物）。
pub fn stdin() -> Stdin {
    Stdin
}

/// 获取标准输出句柄（std::io::stdout() 对应物）。
pub fn stdout() -> Stdout {
    Stdout
}

// ── File 视图（非阻塞，devfs 挂载；envcall 经 filetable 走此路径）──

impl File for Stdin {
    /// 非阻塞单次尝试读 preferred 输入源 — 缓冲空返回
    /// [`FileError::WouldBlock`]，由调用方（envcall / filetable）决定等待方式。
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        // SAFETY: File::read 的 buf 是 &mut [u8]，天然保证可写 buf.len() 字节。
        unsafe { read_to(buf.as_mut_ptr(), buf.len()) }
    }
}

impl File for Stdout {
    /// 经 `print::write` 写 preferred 输出源 — 与 println! 共享 OUT 锁仲裁。
    /// 文本视图：非 UTF-8 字节丢弃（教学取舍，与 println! 同路径）。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        let s = core::str::from_utf8(buf).unwrap_or("");
        print::write(format_args!("{}", s));
        Ok(buf.len())
    }
}

// ── Stdin 阻塞句柄（内核任务 std::io 形态）──

impl Stdin {
    /// 阻塞读取，尽量填满 `buf`，返回实际读取字节数（≥1）。
    ///
    /// 经 `filetable::read(0)` 从 VFS 读取（fd 0 访问模式校验）；缓冲空时
    /// 置 WaitRead → park（`schedule::input_wait`），字符到达后唤醒重试。
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match filetable::read(0, buf) {
                Ok(n) => return Ok(n),
                Err(FileError::WouldBlock) => schedule::input_wait(),
                Err(e) => return Err(e),
            }
        }
    }

    /// 阻塞读取单字节。
    pub fn read_byte(&self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read(&mut b)?;
        Ok(b[0])
    }

    /// 阻塞读取一行：逐字节到 `\n`（含）或 buf 满。
    ///
    /// buf 满而未遇换行时返回已读字节，剩余字符留在缓冲，下次续读。
    pub fn read_line(&self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            let mut b = [0u8; 1];
            match filetable::read(0, &mut b) {
                Ok(1) => {
                    buf[n] = b[0];
                    n += 1;
                    if b[0] == b'\n' {
                        break;
                    }
                }
                Ok(_) => break,
                Err(FileError::WouldBlock) => schedule::input_wait(),
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }
}

// ── Stdout 阻塞句柄（内核任务 std::io 形态）──

impl Stdout {
    /// 经 `filetable::write(1)` 写 preferred 输出源（VFS 强制访问模式）。
    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        filetable::write(1, buf)
    }

    /// 冲刷输出 — 无缓冲层（直写设备），no-op 语义占位（std::io::Write::flush 对应物）。
    pub fn flush(&self) -> Result<()> {
        Ok(())
    }
}

impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        filetable::write(1, s.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

impl fmt::Write for &Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        filetable::write(1, s.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

// ── Read/Write 流契约（ops）——委托固有方法 ──────────────

impl Read for Stdin {
    /// 阻塞读取（委托固有方法，UFCS 显式避免与方法自身歧义）。
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Stdin::read(self, buf)
    }
}

impl Write for Stdout {
    /// 阻塞写入（委托固有方法，经 filetable::write(1)）。
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        Stdout::write(self, buf)
    }

    /// 冲刷 — 无缓冲层 no-op（委托固有方法）。
    fn flush(&mut self) -> Result<()> {
        Stdout::flush(self)
    }
}

// ── 私有辅助 ─────────────────────────────────────────────

/// 尝试从 preferred 输入缓冲读取到裸指针，尽量填满，返回读取字节数。
///
/// **非阻塞单次尝试**：缓冲空返回 [`FileError::WouldBlock`]；有数据则尽量
/// 填（≥1 字节）。供 `File for Stdin` 与 envcall 路径共用。
///
/// # Safety
///
/// 调用方必须保证 `buf` 指向至少 `count` 字节的可写内存。
unsafe fn read_to(buf: *mut u8, count: usize) -> Result<usize> {
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
