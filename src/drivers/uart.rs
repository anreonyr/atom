// NS16550A UART（QEMU virt 固定地址 0x1000_0000）

use core::fmt;

use crate::drivers::PLIC;
use crate::hal::{InterruptController, IrqHandler};
use crate::trap;

pub const UART_BASE: usize = 0x1000_0000;
pub const UART_IRQ: u32 = 10;

pub struct Uart {
    base: *mut u8,
}

// 单核嵌入式环境，裸指针安全
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

    pub const fn new(base: usize) -> Self {
        Self { base: base as *mut u8 }
    }

    /// 初始化 UART 硬件：禁中断 → 使能 FIFO → 设波特率 → 8N1
    pub fn init_hw(&self) {
        let divisor = (Self::CLOCK / (16 * Self::BAUD)) as u16;
        unsafe {
            self.write(Self::IER, 0x00);
            self.write(
                Self::FCR,
                Self::FCR_ENABLE | Self::FCR_CLR_RX | Self::FCR_CLR_TX | Self::FCR_TRIG_14,
            );
            self.write(Self::LCR, Self::LCR_DLAB);
            self.write(Self::DLL, (divisor & 0xFF) as u8);
            self.write(Self::DLM, ((divisor >> 8) & 0xFF) as u8);
            self.write(Self::LCR, 0x03);
        }
    }

    /// 一次完成中断路径配置：PLIC 路由 + 设备 IRQ 使能 + 注册
    pub fn init_irq(&'static self) {
        PLIC.set_priority(UART_IRQ, 1);
        PLIC.enable(UART_IRQ);
        self.enable_irq();
        trap::register_irq(self);
    }

    #[inline]
    pub(crate) unsafe fn read(&self, offset: usize) -> u8 {
        self.base.add(offset).read_volatile()
    }

    #[inline]
    pub(crate) unsafe fn write(&self, offset: usize, val: u8) {
        self.base.add(offset).write_volatile(val)
    }

    /// 直接写入一个字节到 UART（无锁，轮询 THRE）。
    ///
    /// panic handler 专用——绕过 print/log 的 SpinLock 避免死锁。
    pub(crate) fn putc_raw(&self, c: u8) {
        while unsafe { self.read(Self::LSR) } & Self::LSR_THRE == 0 {}
        unsafe { self.write(Self::THR, c) }
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

impl IrqHandler for Uart {
    fn irq_number(&self) -> u32 {
        UART_IRQ
    }

    fn handle_irq(&self) {
        let c = unsafe { self.read(Self::RBR) };
        if c == b'\r' {
            self.putc_raw(b'\r');
        }
        self.putc_raw(c);
    }

    fn enable_irq(&self) {
        unsafe { self.write(Self::IER, Self::IER_RX) }
    }
}

pub static UART: Uart = Uart::new(UART_BASE);
