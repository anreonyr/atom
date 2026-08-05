// UART 服务集成层 — 串口设备能力的注册表 + File/Write/InterruptHandler 适配
//
// 硬件能力契约 `trait Uart` 定义在 hal/uart.rs（hal = 全部硬件能力契约），
// 本层只承载服务集成，是 UART 驱动（driver/uart/）与消费方（filesystem 的
// devfs、sink/source 的设备选择）之间的共享层，依赖 file.rs（File 契约）+ hal：
//   - driver 实现 `hal::uart::Uart`（硬件能力），probe 时经 `register()` 注册
//   - 注册表层构造三视图：UartFile（VFS File 视图，read 从输入缓冲取）、
//     UartWriter（console 输出视图）、InputHandler（中断路由视图，搬运 +
//     回显 + 唤醒）——驱动不接触 File 等类型，文件/终端语义集中在本层
//   - `register()` 三联动：sink 设备选择层（打印设备 "uartN"）、source 设备
//     选择层（输入设备 "uartN"）、trap 中断处理器——消费方不重新发现设备，
//     复用本表同源实例
//
// 对应 Linux：`tty_driver` + 串口 core（tty_port）的注册表面；驱动只实现
// 硬件操作（原子收发），文件/终端语义（回显、缓冲、阻塞读）由 core 提供。

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::file::{File, FileError, Result};
use crate::hal::InterruptHandler;
use crate::lock::SpinLock;
use crate::source::InputBuffer;

// 硬件能力契约重导出：driver/uart/ 的 `impl Uart` 引用本路径（API 兼容）
pub use crate::hal::uart::Uart;

// ── Uart → File / Write / InterruptHandler 三视图（本地包装，孤儿规则）──

/// UART 文件视图 — `File` 的本地包装（持有 uart + 输入缓冲）。
///
/// `fmt::Write`/`File` 是外部 trait，不能为裸类型参数实现（孤儿规则 E0210），
/// 故以本地新类型承载视图。read 从 [`InputBuffer`] 取（非阻塞：空 →
/// [`FileError::WouldBlock`]，不再轮询 RBR——中断已把字符搬入缓冲）；
/// write 转 `write_with_crlf`。
pub struct UartFile<U: Uart + 'static> {
    uart: &'static U,
    input: &'static InputBuffer,
}

impl<U: Uart + 'static> File for UartFile<U> {
    /// 尝试从输入缓冲读取一个字节（非阻塞：无数据立即返回 WouldBlock）。
    ///
    /// 字节设备无 EOF；`Ok(0)` 仅表示空缓冲请求。调用方应处理
    /// [`FileError::WouldBlock`]（重试或等待中断），不得轮询忙等。
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(c) = self.input.pop() else {
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
                unsafe { self.uart.write_byte(b) }
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

// ── UART 注册表 ───────────────────────────────────────────

/// 已注册 UART — 同一实例的 File（devfs）、Write（console）与 input 三视图。
#[derive(Clone, Copy)]
pub struct SerialDevice {
    /// VFS 文件能力（devfs 节点挂载）
    pub file: &'static dyn File,
    /// 输出能力（console writer）— 当前经 sink 联动注册消费，字段为 devfs
    /// 之外的共享视图预留（如未来 /dev/console 打开目标）。
    #[allow(dead_code)]
    pub writer: &'static dyn fmt::Write,
    /// 输入能力（输入缓冲 — 中断侧 push / 读取侧 pop）
    pub input: &'static InputBuffer,
}

/// UART 注册表 — 按 probe（设备发现）顺序排列。
static UARTS: SpinLock<Vec<SerialDevice>> = SpinLock::new(Vec::new());

/// 注册一个 UART 实例（驱动 probe 内调用，Linux 注册 tty 设备的对应物）。
///
/// 注册时构造 File/Write/input 三视图与中断处理器——驱动只传自身实例，
/// 不接触 File 等类型。三联动：
///   1. sink 设备选择层（打印设备 "uartN"）——首个自动 preferred（console）
///   2. source 设备选择层（输入设备 "uartN"）——首个自动 preferred（stdin）
///   3. trap 中断处理器（InputHandler：中断字符搬运 → 缓冲 + 回显 + 唤醒）
/// 序号 N 与 devfs /dev/consoleN 一致（消费方不重新发现设备）。
pub fn register<U: Uart + 'static>(uart: &'static U) {
    // 三视图（本地包装，孤儿规则）+ 输入缓冲，泄漏为 'static 供长期持有。
    let writer: &'static dyn fmt::Write = Box::leak(Box::new(UartWriter(uart)));
    let input: &'static InputBuffer = Box::leak(Box::new(InputBuffer::new()));
    let file: &'static dyn File = Box::leak(Box::new(UartFile { uart, input }));

    let mut uarts = UARTS.lock();
    let idx = uarts.len();
    uarts.push(SerialDevice { file, writer, input });
    drop(uarts);

    let name: &'static str = Box::leak(alloc::format!("uart{idx}").into_boxed_str());
    // 联动 1：打印设备（sink 输出选择层；首个自动 preferred = console）
    let _ = crate::sink::register(crate::sink::SinkDevice::new(name, writer));
    // 联动 2：输入设备（source 输入选择层；首个自动 preferred = stdin）
    let _ = crate::source::register(crate::source::InputDevice::new(name, input));
    // 联动 3：中断处理器（搬运 → 缓冲 + 回显 + 唤醒）
    let handler: &'static dyn InterruptHandler = Box::leak(Box::new(InputHandler { uart, input }));
    crate::trap::register_interrupt_handler(handler);
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
