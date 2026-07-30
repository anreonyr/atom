// NS16550A UART — MMIO 基址从 platform config 获取

use core::fmt;

use crate::drivers::PLIC;
use crate::hal::{Driver, DriverError, ExternalInterrupt, InterruptHandler, Mmio};
use crate::lock::OnceLock;
use crate::trap;

#[derive(Debug)]
pub struct Uart {
    base: *mut u8,
    interrupt: u32,
}

// 单核嵌入式环境，裸指针安全
unsafe impl Send for Uart {}
unsafe impl Sync for Uart {}

impl Uart {
    // ── 寄存器偏移 ─────────────────────────────────────────
    const RBR: usize = 0x0;
    const THR: usize = 0x0;
    const IER: usize = 0x1;
    const FCR: usize = 0x2;
    const LCR: usize = 0x3;
    const LSR: usize = 0x5;

    // ── 时钟 / 波特率 ─────────────────────────────────────
    const CLOCK: u32 = 11_059_200;
    const BAUD: u32 = 115_200;
    const DLL: usize = 0x0;
    const DLM: usize = 0x1;

    // ── LCR ────────────────────────────────────────────────
    const LCR_DLAB: u8 = 0x80;

    // ── FCR ────────────────────────────────────────────────
    const FCR_ENABLE: u8 = 0x01;
    const FCR_CLR_RX: u8 = 0x02;
    const FCR_CLR_TX: u8 = 0x04;
    const FCR_TRIG_14: u8 = 0xC0;

    // ── IER ────────────────────────────────────────────────
    const IER_RX: u8 = 0x01;

    // ── LSR ────────────────────────────────────────────────
    const LSR_THRE: u8 = 0x20;

    // ─────────────────────────────────────────────────────

    pub const fn new(base: usize, interrupt: u32) -> Self {
        Self {
            base: base as *mut u8,
            interrupt,
        }
    }

    /// 直接写入一个字节到 UART（无锁，轮询 THRE）。
    ///
    /// panic handler 专用——绕过 print/log 的 SpinLock 避免死锁。
    pub(crate) fn putc_raw(&self, c: u8) {
        while unsafe { self.read(Self::LSR) } & Self::LSR_THRE == 0 {}
        unsafe { self.write(Self::THR, c) }
    }
}

impl Mmio for Uart {
    type T = u8;

    fn base(&self) -> *mut u8 {
        self.base
    }
}

impl Driver for Uart {
    fn name(&self) -> &'static str {
        "ns16550a"
    }

    #[allow(static_mut_refs)]
    fn init(&self) -> Result<(), DriverError> {
        // 硬件初始化：波特率 / FIFO / 8N1
        let this = &self;
        let divisor = (Uart::CLOCK / (16 * Uart::BAUD)) as u16;
        unsafe {
            this.write(Uart::IER, 0x00);
            this.write(
                Uart::FCR,
                Uart::FCR_ENABLE | Uart::FCR_CLR_RX | Uart::FCR_CLR_TX | Uart::FCR_TRIG_14,
            );
            this.write(Uart::LCR, Uart::LCR_DLAB);
            this.write(Uart::DLL, (divisor & 0xFF) as u8);
            this.write(Uart::DLM, ((divisor >> 8) & 0xFF) as u8);
            this.write(Uart::LCR, 0x03);
        }

        // 中断路由：PLIC 优先级 + 使能 + 设备 IER + 注册 handler
        let interrupt = self.interrupt;
        unsafe {
            PLIC().set_priority(interrupt, 1);
            PLIC().enable(interrupt);
            self.write(Self::IER, Self::IER_RX);
            // SAFETY: self 来自 pub static mut UART，实际就是 'static。
            // 此处 transmute 是因为 trait 签名 `fn init(&self)` 不携带 'static 信息。
            let static_self: &'static Self = core::mem::transmute(self);
            trap::register_interrupt_handler(static_self);
        }
        Ok(())
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.putc_raw(b'\r');
            }
            self.putc_raw(b);
        }
        Ok(())
    }
}

impl InterruptHandler for Uart {
    fn interrupt_number(&self) -> u32 {
        self.interrupt
    }

    fn handle_interrupt(&self) {
        let c = unsafe { self.read(Self::RBR) };
        if c == b'\r' {
            self.putc_raw(b'\r');
        }
        self.putc_raw(c);
    }

    fn enable_interrupt(&self) {
        unsafe { self.write(Self::IER, Self::IER_RX) }
    }
}

static UART_INSTANCE: OnceLock<Uart> = OnceLock::new();

/// 初始化 UART 实例（引导早期调用一次）
pub(crate) fn init(base: usize, interrupt: u32) {
    UART_INSTANCE
        .set(Uart::new(base, interrupt))
        .expect("UART already initialized");
}

/// 从 DTB 发现缓冲区创建 UART 实例。
pub(crate) fn probe() {
    if let Some(dev) = crate::platform::find_device("ns16550a") {
        init(dev.base, dev.interrupt.unwrap_or(10));
    }
}

/// 获取 UART 实例引用
#[allow(non_snake_case)]
pub fn UART() -> &'static Uart {
    UART_INSTANCE.get().expect("UART not initialized")
}
