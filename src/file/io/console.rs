// file/io/console — 终端核心（设备无关的终端服务）
//
// 分层：hal::ByteChannel（设备能力契约）← console（终端核心）← print（输出路由）
// console 把「字节收发 + 中断」的原始设备组装为终端语义（Linux tty core 对应物：
// 驱动只做原子收发，终端语义在此）：
//
//   - `Console`：终端核心节点（InputBuffer + 回显/唤醒 + 持有 `&dyn ByteChannel`
//     能力引用），实现 File（/dev/consoleN 终端视图）+ `insert_char` 服务。
//     不实现 InterruptHandler——中断 handler 归设备，见 `InputHandler`。
//   - `InputHandler`：设备中断处理器（响应硬件 RX 中断，搬 FIFO →
//     `Console::insert_char` → 唤醒）。归设备不归终端（Linux：UART 驱动的
//     irq handler 搬字符 → tty 层缓冲/回显）。
//   - `RawFile`：/dev/uartN 原始字节流视图（无终端语义：write 无 CRLF 转换、
//     read 共享输入缓冲）。
//
// `register(device: &'static dyn ByteChannel)` 非泛型——新终端设备只需实现
// ByteChannel 能力即可复用整套终端服务（virtio-console 等）。
//
// 无锁输出（panic / lockdep）不经本模块——调用方直接 `sbi::mprintln!`。

use alloc::boxed::Box;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::file::ops::{File, FileError, Result};
use crate::file::registry;
use crate::hal::byte_channel::ByteChannel;
use crate::hal::InterruptHandler;
use crate::lock::SpinLock;

// ── 输入缓冲 ─────────────────────────────────────────────

/// 输入缓冲容量（字节）— 定长环形，满时丢弃新字符（保旧）。
pub const INPUT_BUFFER_CAPACITY: usize = 256;

/// 输入缓冲 — SpinLock 保护定长环形缓冲。
///
/// push 由中断上下文（InputHandler）调用；pop 由读取侧（devfs File）调用。
/// 满时丢弃新字符（终端输入场景保旧更合理）。
pub struct InputBuffer {
    inner: SpinLock<Ring>,
}

struct Ring {
    data: [u8; INPUT_BUFFER_CAPACITY],
    /// 下一个写入位置（环形索引）
    head: usize,
    /// 已缓冲字节数
    len: usize,
}

impl InputBuffer {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(Ring {
                data: [0; INPUT_BUFFER_CAPACITY],
                head: 0,
                len: 0,
            }),
        }
    }

    /// 入队一个字节（满则丢弃新字符）。
    ///
    /// 返回是否发生"空 → 非空"转变——调用方（InputHandler）据此决定是否
    /// 唤醒输入等待者（等待者只在缓冲空时阻塞，非空时 pop 直接成功）。
    pub fn push(&self, c: u8) -> bool {
        let mut ring = self.inner.lock();
        let was_empty = ring.len == 0;
        if ring.len == INPUT_BUFFER_CAPACITY {
            return false; // 满：丢弃新字符
        }
        let head = ring.head;
        ring.data[head] = c;
        ring.head = (head + 1) % INPUT_BUFFER_CAPACITY;
        ring.len += 1;
        was_empty
    }

    /// 出队一个字节（非阻塞；空返回 `None`）。
    pub fn pop(&self) -> Option<u8> {
        let mut ring = self.inner.lock();
        if ring.len == 0 {
            return None;
        }
        let tail = (ring.head + INPUT_BUFFER_CAPACITY - ring.len) % INPUT_BUFFER_CAPACITY;
        let c = ring.data[tail];
        ring.len -= 1;
        Some(c)
    }

    /// 当前缓冲字节数。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().len
    }

    /// 是否为空。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().len == 0
    }
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ── 终端核心 ─────────────────────────────────────────────

/// 终端核心 — 设备无关的终端语义对象（`/dev/consoleN` 挂载）。
///
/// 持有 `&dyn ByteChannel` 字节收发能力引用 + 输入缓冲 + 回显开关；提供
/// File 视图（read 从缓冲 pop、write 逐字节 CRLF 写出）与 `insert_char`
/// 中断喂字符服务（缓冲 + 回显 + 空→非空判定）。不实现 InterruptHandler
/// （中断响应归设备，见 [`InputHandler`]）。
pub struct Console {
    device: &'static dyn ByteChannel,
    input: InputBuffer,
    echo: bool,
}

impl Console {
    /// 中断喂入一个字节 — 终端服务：入缓冲 + 回显（`\r` 特判保留）+ 返回
    /// 空→非空转变（调用方据此决定是否唤醒输入等待者）。
    pub fn insert_char(&self, c: u8) -> bool {
        let woke = self.input.push(c);
        if self.echo {
            // 回显（\r 特判保留——终端回车回显）
            if c == b'\r' {
                // SAFETY: UART MMIO mapped during init; called from trap handler after boot.
                unsafe { self.device.write_byte(b'\r') };
            }
            // SAFETY: UART MMIO mapped during init.
            unsafe { self.device.write_byte(c) };
        }
        woke
    }
}

impl File for Console {
    /// 从输入缓冲非阻塞 pop（缓冲空返回 [`FileError::WouldBlock`]，同 stdin 语义）。
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

    /// 逐字节写出（`\n` → `\r\n`）— 终端输出语义。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        write_with_crlf(
            &mut |b| {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.device.write_byte(b) }
            },
            buf,
        );
        Ok(buf.len())
    }
}

// ── 设备中断处理器 ─────────────────────────────────────────

/// 设备输入中断处理器 — 归设备，不归终端。
///
/// 响应硬件 RX 中断（`interrupt_number` = 设备 PLIC 中断号），把硬件 FIFO
/// 所有就绪字符搬入终端核心（`Console::insert_char` 处理缓冲/回显），
/// 空→非空时唤醒输入等待者（`schedule::wake_input_waiters`）。
pub struct InputHandler {
    device: &'static dyn ByteChannel,
    console: &'static Console,
}

impl InterruptHandler for InputHandler {
    fn interrupt_number(&self) -> u32 {
        self.device.interrupt_number()
    }

    fn handle_interrupt(&self) {
        let mut woke = false;
        while let Some(c) = self.device.read_byte() {
            woke |= self.console.insert_char(c);
        }
        if woke {
            crate::schedule::wake_input_waiters();
        }
    }
}

// ── 原始字节流视图 ─────────────────────────────────────────

/// 原始字节流 File — `/dev/uartN` 挂载（无终端语义）。
///
/// read 共享对应终端的输入缓冲（数据已被中断搬入）；write 直写设备、
/// **无 CRLF 转换**（原始编程接口语义）。不经 print 层 OUT 锁——原始设备
/// 访问可接受与 console 并发写的字节交错。
pub struct RawFile {
    device: &'static dyn ByteChannel,
    input: &'static InputBuffer,
}

impl File for RawFile {
    /// 从输入缓冲非阻塞 pop（同终端 read 语义；空返回 WouldBlock）。
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

    /// 直写设备，无 CRLF 转换（原始字节流）。
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize> {
        for &b in buf {
            // SAFETY: MMIO region is identity-mapped during driver init.
            unsafe { self.device.write_byte(b) }
        }
        Ok(buf.len())
    }
}

// ── 注册 ─────────────────────────────────────────────────

/// 已注册终端数 — 启动日志与诊断用。
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// 已注册的终端数。
pub fn count() -> usize {
    COUNT.load(Ordering::Relaxed)
}

/// 注册一个字节通道设备（驱动 probe 内调用，Linux 注册 tty 设备的对应物）。
///
/// 非泛型入口——设备无关：构造终端核心（Console + InputHandler + RawFile）
/// 并三联动注册：
///   1. 复用器注册表：`/dev/consoleN`（终端 File）+ `/dev/uartN`（原始字节流）
///   2. trap 中断处理器（InputHandler：设备 RX 中断 → 搬运 → 缓冲 + 回显 + 唤醒）
///
/// devfs 的 `/dev/console → consoleN` 符号链接在 `create_devfs` 时按首个
/// consoleN 建立（preferred 表达，见 file/devfs/mod.rs）。
pub fn register(device: &'static dyn ByteChannel) {
    let idx = COUNT.fetch_add(1, Ordering::Relaxed);

    // 终端核心：InputBuffer 由 Console 字段持有（&Console 的字段 reborrow 得
    // 'static 共享，RawFile/InputHandler 复用同一缓冲，无需独立泄漏）。
    let console: &'static Console =
        Box::leak(Box::new(Console { device, input: InputBuffer::new(), echo: true }));

    // /dev/consoleN（终端 File）
    let file: &'static dyn File = console;
    let console_path: &'static str = Box::leak(alloc::format!("/dev/console{idx}").into_boxed_str());
    let _ = registry::register(console_path, file);

    // /dev/uartN（原始字节流，共享输入缓冲——须在 leak(console) 之后构造）
    let raw: &'static RawFile =
        Box::leak(Box::new(RawFile { device, input: &console.input }));
    let raw_path: &'static str = Box::leak(alloc::format!("/dev/uart{idx}").into_boxed_str());
    let _ = registry::register(raw_path, raw);

    // 设备中断处理器（搬运 → 缓冲 + 回显 + 唤醒）
    let handler: &'static dyn InterruptHandler =
        Box::leak(Box::new(InputHandler { device, console }));
    crate::trap::register_interrupt_handler(handler);
}

// ── 输出辅助 ─────────────────────────────────────────────

/// 逐字节写出，`\n` 自动前置 `\r`（终端通用输出语义：回车换行）。
///
/// 型号差异（寄存器布局/忙等位）由调用方传入的单字节回调承担。
pub fn write_with_crlf<F: FnMut(u8)>(out: &mut F, buf: &[u8]) {
    for &b in buf {
        if b == b'\n' {
            out(b'\r');
        }
        out(b);
    }
}
