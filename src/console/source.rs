// 输入设备选择层 — 输入目标路由表（对应 sink.rs 的输出设备选择层）
//
// 本层回答一个问题："输入从哪个设备来"。与 sink.rs 对称：
//   sink.rs   输出选择层：SinkDevice { name, writer } + preferred + SBI 兜底（恒非空）
//   source.rs 输入选择层：InputDevice { name, buffer } + preferred（无兜底 → Option）
//
// 缓冲（InputBuffer）挂在设备条目上（对应 SinkDevice.writer 句柄）：中断侧
// InputHandler 拿 &'static InputBuffer 做 push（空→非空时唤醒输入等待者），
// 读取侧（console::input / devfs File）从 buffer pop。
//
// 设备来源复用能力契约层：UART 驱动 probe 时经 uart::register 联动注册，
// source 不重新发现设备（与 sink 同构）。输入设备只增不删（无 unregister）。
//
// 对应 Linux：tty 输入缓冲（tty_buffer）挂在 tty_port 上，console 读取走
// 该缓冲；多个输入设备时 preferred 决定"哪个是 stdin"。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::lock::SpinLock;

/// 输入缓冲容量（字节）— 定长环形，满时丢弃新字符（保旧）。
pub const INPUT_BUFFER_CAPACITY: usize = 256;

/// 输入缓冲 — SpinLock 保护定长环形缓冲。
///
/// push 由中断上下文（InputHandler）调用；pop 由读取侧（console::input /
/// devfs File）调用。满时丢弃新字符（终端输入场景保旧更合理）。
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

// ── 注册表 ──────────────────────────────────────────────

/// 输入设备 — 注册表条目：身份 + 输入缓冲句柄。
pub struct InputDevice {
    /// 设备身份（与 sink 的打印设备同名：uartN）
    pub name: &'static str,
    /// 输入缓冲（中断侧 push / 读取侧 pop 的共享状态）
    pub buffer: &'static InputBuffer,
}

impl InputDevice {
    /// 构造输入设备条目。
    pub fn new(name: &'static str, buffer: &'static InputBuffer) -> Self {
        InputDevice { name, buffer }
    }
}

/// 输入设备注册错误
///
/// NameTaken 的载荷当前无读取方（register 的调用方丢弃错误），保留以携带诊断信息。
#[allow(dead_code)]
#[derive(Debug)]
pub enum SourceError {
    /// 注册时名字已存在
    NameTaken(&'static str),
}

/// 输入设备注册表 — 按注册（设备发现）顺序排列。
static DEVICES: SpinLock<Vec<InputDevice>> = SpinLock::new(Vec::new());

/// preferred 输入设备索引（PREFERRED_INVALID = 未注册任何输入设备）。
const PREFERRED_INVALID: usize = usize::MAX;
static PREFERRED: AtomicUsize = AtomicUsize::new(PREFERRED_INVALID);

/// 注册输入设备 — 首个注册自动成为 preferred（stdin 语义）。
///
/// UART 驱动 probe 时经 `uart::register` 联动调用；名字冲突返回
/// [`SourceError::NameTaken`]。
pub fn register(dev: InputDevice) -> Result<(), SourceError> {
    let mut list = DEVICES.lock();
    if list.iter().any(|d| d.name == dev.name) {
        return Err(SourceError::NameTaken(dev.name));
    }
    let idx = list.len();
    list.push(dev);
    // 首个注册自动成为 preferred（输入侧无 SBI 兜底，表空 → None）
    let _ =
        PREFERRED.compare_exchange(PREFERRED_INVALID, idx, Ordering::Relaxed, Ordering::Relaxed);
    Ok(())
}

/// 当前 preferred 输入缓冲（首个注册的输入设备；未注册 → None）。
///
/// 输入侧无 SBI 类常驻兜底，返回 `Option`——调用方（console::input::read）
/// 无设备时返回错误，避免永久阻塞。
pub fn preferred() -> Option<&'static InputBuffer> {
    let list = DEVICES.lock();
    let idx = PREFERRED.load(Ordering::Relaxed);
    // `dev.buffer` 是 'static 引用（register 时 Box::leak），拷贝即可脱离锁生命周期。
    list.get(idx).map(|dev| dev.buffer)
}
