// UART 服务集成层 — 串口设备能力的注册 + Write/Input/InterruptHandler 适配
//
// 硬件能力契约 `trait Uart` 定义在 hal/uart.rs（hal = 全部硬件能力契约），
// 本层只承载服务集成，是 UART 驱动（driver/uart/）与消费方（io::device 设备
// 选择层、中断路由）之间的共享层，依赖 file.rs 契约 + hal：
//   - driver 实现 `hal::uart::Uart`（硬件能力），probe 时经 `register()` 注册
//   - 注册表构造两视图 + 中断处理器：UartWriter（console 输出视图）、
//     InputBuffer（输入缓冲——中断侧 push / 读取侧 pop）、InputHandler
//     （中断路由视图，搬运 + 回显 + 唤醒）——驱动不接触 File 等类型，
//     终端语义集中在本层
//   - `register()` 联动：io::device 设备选择层（device::register，一次构造
//     读写双视图条目 "uartN"）、trap 中断处理器——消费方不重新发现设备
//
// 对应 Linux：`tty_driver` + 串口 core（tty_port）的注册表面；驱动只实现
// 硬件操作（原子收发），文件/终端语义（回显、缓冲、阻塞读）由 core 提供。

use alloc::boxed::Box;
use core::fmt;

use crate::file::ops::{File, FileError, Result};
use crate::file::registry;
use super::device::InputBuffer;
use crate::hal::InterruptHandler;

// 硬件能力契约重导出：driver/uart/ 的 `impl Uart` 引用本路径（API 兼容）
pub use crate::hal::uart::Uart;

// ── Uart → Write / Input / InterruptHandler 视图（本地包装，孤儿规则）──

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

/// UART 输入中断处理器 — 搬运 + 回显 + 唤醒（Linux 驱动私有 irq handler 搬
/// 字符 + n_tty 回显的对应物；两个型号的回显逻辑从驱动上移至此集中为一份）。
///
/// `handle_interrupt`：把硬件 FIFO 所有就绪字符搬入 [`InputBuffer`]
/// （对应 `uart_insert_char`），空→非空时唤醒输入等待者
/// （`schedule::wake_input_waiters`）；回显（echo）在此统一（`\r` 特判
/// 保留——终端回车回显）。
pub struct InputHandler<U: Uart + 'static> {
    uart: &'static U,
    input: &'static InputBuffer,
}

impl<U: Uart + 'static> InterruptHandler for InputHandler<U> {
    fn interrupt_number(&self) -> u32 {
        self.uart.interrupt_number()
    }

    fn handle_interrupt(&self) {
        let mut woke = false;
        while let Some(c) = self.uart.read_byte() {
            // 空→非空转变（可能有任务阻塞在等待）→ 记录待唤醒
            woke |= self.input.push(c);
            // 回显（\r 特判保留——终端回车回显）
            if c == b'\r' {
                // SAFETY: UART MMIO mapped during init; called from trap handler after boot.
                unsafe { self.uart.write_byte(b'\r') };
            }
            // SAFETY: UART MMIO mapped during init.
            unsafe { self.uart.write_byte(c) };
        }
        // 缓冲空→非空：唤醒输入等待者（调度器 WaitRead 原语）
        if woke {
            crate::schedule::wake_input_waiters();
        }
    }
}

/// UART 文件视图 — `/dev/consoleN` 挂载（File 契约）。
///
/// read 从输入缓冲非阻塞 pop（缓冲空返回 [`FileError::WouldBlock`]，同
/// stdin 语义）；write 逐字节写出（`\n` → `\r\n`），与 UartWriter 同机制
/// （`&Uart` 共享引用写 MMIO，无别名问题）。
pub struct UartFile<U: Uart + 'static> {
    uart: &'static U,
    input: &'static InputBuffer,
}

impl<U: Uart + 'static> File for UartFile<U> {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            let Some(c) = self.input.pop() else { break };
            buf[n] = c;
            n += 1;
        }
        if n == 0 {
            return Err(FileError::WouldBlock);
        }
        Ok(n)
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        write_with_crlf(
            &mut |b| {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.uart.write_byte(b) }
            },
            buf,
        );
        Ok(buf.len())
    }
}

// ── 注册 ─────────────────────────────────────────────────

/// 注册一个 UART 实例（驱动 probe 内调用，Linux 注册 tty 设备的对应物）。
///
/// 注册时构造 Write/Input/File 视图与中断处理器——驱动只传自身实例，不接触
/// File 等类型。三联动：
///   1. console 设备选择层（device::register：一次注册读写双视图条目
///      "uartN"）——首个自动 preferred（stdin/stdout 同一设备）
///   2. 复用器注册表（registry::register：/dev/consoleN——devfs 枚举建节点）
///   3. trap 中断处理器（InputHandler：中断字符搬运 → 缓冲 + 回显 + 唤醒）
pub fn register<U: Uart + 'static>(uart: &'static U) {
    // 两视图（本地包装，孤儿规则）+ 输入缓冲，泄漏为 'static 供长期持有。
    let writer: &'static dyn fmt::Write = Box::leak(Box::new(UartWriter(uart)));
    let input: &'static InputBuffer = Box::leak(Box::new(InputBuffer::new()));

    // 统一设备表：一次构造读写双视图条目（原 sink/source 两表各注册一次的
    // 冗余消除）。序号 = 已注册 UART 数（不含常驻 sbi），与 device::count() 一致。
    let idx = super::device::count();
    let name: &'static str = Box::leak(alloc::format!("uart{idx}").into_boxed_str());
    let _ = super::device::register(super::device::ConsoleDevice::new(
        name,
        Some(writer),
        Some(input),
    ));
    // 复用器注册表：/dev/consoleN（devfs 枚举建节点）
    let file: &'static dyn File = Box::leak(Box::new(UartFile { uart, input }));
    let console_path: &'static str =
        Box::leak(alloc::format!("/dev/console{idx}").into_boxed_str());
    let _ = registry::register(console_path, file);
    // 中断处理器（搬运 → 缓冲 + 回显 + 唤醒）
    let handler: &'static dyn InterruptHandler = Box::leak(Box::new(InputHandler { uart, input }));
    crate::trap::register_interrupt_handler(handler);
}

/// 逐字节写出，`\n` 自动前置 `\r`（UART 通用输出语义：回车换行）。
///
/// 两个 UART 型号的 fmt::Write::write_str 共用本函数，消除重复；型号差异
/// （寄存器布局/忙等位）由调用方传入的单字节回调承担。
pub fn write_with_crlf<F: FnMut(u8)>(out: &mut F, buf: &[u8]) {
    for &b in buf {
        if b == b'\n' {
            out(b'\r');
        }
        out(b);
    }
}
