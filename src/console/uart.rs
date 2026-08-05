// UART 服务集成层 — 串口设备能力的注册表 + File/Write/InterruptHandler 适配
//
// 硬件能力契约 `trait Uart` 定义在 hal/uart.rs（hal = 全部硬件能力契约），
// 本层只承载服务集成，是 UART 驱动（driver/uart/）与消费方（filesystem 的
// devfs、sink 的设备选择）之间的共享层，依赖 file.rs（File 契约）+ hal：
//   - driver 实现 `hal::uart::Uart`（硬件能力），probe 时经 `register()` 注册
//   - 注册表层提供 blanket 适配：Uart 自动成为 `File`（VFS 视图）、
//     `fmt::Write`（console 输出视图）、`InterruptHandler`（中断路由视图）
//   - 驱动不接触 File 类型——文件语义（非阻塞读 / `\r\n` 转义）集中在本层
//   - `register()` 联动注册到 sink 设备选择层（打印设备 "uartN"，首个自动
//     成为 preferred）——sink 不重新发现设备，复用本表同源实例
//
// 对应 Linux：`tty_driver` + 串口 core（tty_port）的注册表面；驱动只实现
// 硬件操作，文件/终端语义由 core 提供。

use alloc::vec::Vec;
use core::fmt;

use crate::file::{File, FileError, Result};
use crate::hal::InterruptHandler;
use crate::lock::SpinLock;

// 硬件能力契约重导出：driver/uart/ 的 `impl Uart` 引用本路径（API 兼容）
pub use crate::hal::uart::Uart;

// ── blanket 适配：Uart → File / Write / InterruptHandler ──

impl<U: Uart + ?Sized> File for U {
    /// 尝试从 UART 读取一个字节（非阻塞：无数据立即返回 WouldBlock）。
    ///
    /// 字节设备无 EOF；`Ok(0)` 仅表示空缓冲请求。调用方应处理
    /// [`FileError::WouldBlock`]（重试或等待中断），不得轮询忙等。
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(c) = self.read_byte() else {
            return Err(FileError::WouldBlock);
        };
        buf[0] = c;
        Ok(1)
    }

    /// 向 UART 写入字节（`\n` 自动转换为 `\r\n`）。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        write_with_crlf(
            &mut |b| {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.write_byte(b) }
            },
            buf,
        );
        Ok(buf.len())
    }
}

/// UART 输出视图 — `fmt::Write` 的本地包装。
///
/// `fmt::Write` 是外部 trait，不能为裸类型参数实现（孤儿规则 E0210），
/// 故以本地新类型承载 Write 视图；注册时泄漏为 `&'static` 供 console 使用。
#[derive(Clone, Copy)]
pub struct UartWriter<U: Uart + 'static>(&'static U);

impl<U: Uart + 'static> fmt::Write for UartWriter<U> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_with_crlf(
            &mut |b| {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.0.write_byte(b) }
            },
            s.as_bytes(),
        );
        Ok(())
    }
}

impl<U: Uart + ?Sized> InterruptHandler for U {
    fn interrupt_number(&self) -> u32 {
        Uart::interrupt_number(self)
    }

    fn handle_interrupt(&self) {
        Uart::handle_interrupt(self)
    }
}

// ── UART 注册表 ───────────────────────────────────────────

/// 已注册 UART — 同一实例的 File（devfs）与 Write（console）双视图。
#[derive(Clone, Copy)]
pub struct SerialDevice {
    /// VFS 文件能力（devfs 节点挂载）
    pub file: &'static dyn File,
    /// 输出能力（console writer）— 当前经 sink 联动注册消费，字段为 devfs
    /// 之外的共享视图预留（如未来 /dev/console 打开目标）。
    #[allow(dead_code)]
    pub writer: &'static dyn fmt::Write,
}

/// UART 注册表 — 按 probe（设备发现）顺序排列。
static UARTS: SpinLock<Vec<SerialDevice>> = SpinLock::new(Vec::new());

/// 注册一个 UART 实例（驱动 probe 内调用，Linux 注册 tty 设备的对应物）。
///
/// 注册时构造 File 与 Write 双视图——驱动只传自身实例，不接触 File 类型。
/// 同时联动注册到 sink 设备选择层（打印设备 "uartN"，N 与 devfs /dev/consoleN
/// 序号一致）——首个注册的 UART 自动成为 preferred（console）。
pub fn register<U: Uart + 'static>(uart: &'static U) {
    // Write 视图经 UartWriter 本地包装（孤儿规则），泄漏为 'static 供 console 长期持有。
    let writer: &'static dyn fmt::Write =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(UartWriter(uart)));
    let mut uarts = UARTS.lock();
    let idx = uarts.len();
    uarts.push(SerialDevice {
        file: uart as &'static dyn File,
        writer,
    });
    drop(uarts);
    // 联动：注册为打印设备（sink 不重新发现设备，复用本表同源实例）
    let name: &'static str = alloc::boxed::Box::leak(alloc::format!("uart{idx}").into_boxed_str());
    let _ = crate::sink::register(crate::sink::SinkDevice::new(name, writer));
}

/// 所有已注册 UART（供 devfs 枚举；第一个为 console）。
pub fn all() -> Vec<SerialDevice> {
    UARTS.lock().clone()
}

/// 逐字节写出，`\n` 自动前置 `\r`（UART 通用输出语义：回车换行）。
///
/// 两个 UART 型号的 File::write 与 fmt::Write::write_str 共用本函数，
/// 消除重复；型号差异（寄存器布局/忙等位）由调用方传入的单字节回调承担。
pub fn write_with_crlf<F: FnMut(u8)>(out: &mut F, buf: &[u8]) {
    for &b in buf {
        if b == b'\n' {
            out(b'\r');
        }
        out(b);
    }
}
