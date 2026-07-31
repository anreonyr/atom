// /dev/console — 控制台设备，桥接 UART 硬件
//
// 通过 hub 获取已初始化的 UART 实例，将其暴露为文件系统接口。
// ConsoleDev 是零大小类型，不持有状态——所有操作委托给 hub 中的 UART。

use crate::drivers::hub;
use crate::drivers::uart::Uart;
use crate::filesystem::traits::{FileError, FileRead, FileWrite, Result};
use crate::hal::Mmio;

/// 控制台设备 — 桥接硬件 UART。
///
/// 静态实例，引导期创建，永不释放。
pub struct ConsoleDev;

/// 全局控制台设备实例。
pub static CONSOLE: ConsoleDev = ConsoleDev;

// SAFETY: ConsoleDev 是 ZST，无内部可变状态；trait 方法内部通过
// hub::get 访问 UART（UART 自身是 Sync + 单 hart 操作安全）。
unsafe impl Send for ConsoleDev {}
unsafe impl Sync for ConsoleDev {}

impl FileRead for ConsoleDev {
    /// 从 UART 轮询读取一个字节。
    ///
    /// 阻塞等待直到有数据到达（轮询 LSR bit 0: Data Ready）。
    /// 当前仅支持单字节读取，后续可扩展为缓冲读取。
    fn read(&self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let uart = hub::get::<Uart>("uart-0").ok_or(FileError::IoError)?;

        // 轮询等待数据就绪
        while unsafe { uart.read(Uart::LSR) } & 0x01 == 0 {
            core::hint::spin_loop();
        }

        buf[0] = unsafe { uart.read(Uart::RBR) };
        Ok(1)
    }
}

impl FileWrite for ConsoleDev {
    /// 向 UART 写入字节。
    ///
    /// `\n` 自动转换为 `\r\n`（与现有 `fmt::Write for Uart` 行为一致）。
    /// 忽略 offset（字节设备无文件位置概念）。
    fn write(&self, buf: &[u8]) -> Result<usize> {
        let uart = hub::get::<Uart>("uart-0").ok_or(FileError::IoError)?;
        for &b in buf {
            if b == b'\n' {
                uart.putc_raw(b'\r');
            }
            uart.putc_raw(b);
        }
        Ok(buf.len())
    }
}
