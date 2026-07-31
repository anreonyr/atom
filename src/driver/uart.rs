// NS16550A UART — MMIO 基址从 platform config 获取

use core::fmt;

use super::Driver;
use crate::hal::{ExternalInterrupt, InterruptHandler, Mmio};
use crate::trap;

#[derive(Debug)]
pub struct Uart {
    base: *mut u8,
    interrupt: u32,
}

// SAFETY: single-hart kernel; MMIO base pointer is valid for the lifetime of the system.
unsafe impl Send for Uart {}
unsafe impl Sync for Uart {}

impl Uart {
    // ── 寄存器偏移 ─────────────────────────────────────────
    pub(crate) const RBR: usize = 0x0;
    pub(crate) const THR: usize = 0x0;
    const IER: usize = 0x1;
    const FCR: usize = 0x2;
    const LCR: usize = 0x3;
    pub(crate) const LSR: usize = 0x5;

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

    /// Write a single byte to UART (lock-free, polls THRE).
    ///
    /// Used by the panic handler — bypasses print/log SpinLock to avoid deadlock.
    ///
    /// # Safety
    ///
    /// Caller must ensure the UART MMIO region has been identity-mapped before
    /// calling this function. Calling without a valid MMIO mapping results in a
    /// page fault.
    pub(crate) unsafe fn write_byte(&self, c: u8) {
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
    fn init(&'static self) -> Result<(), super::DriverError> {
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
        let plic = crate::driver::hub::get::<crate::driver::plic::Plic>("plic-0")
            .expect("PLIC not registered before UART init");
        unsafe {
            plic.set_priority(interrupt, 1);
            plic.enable(interrupt);
            self.write(Self::IER, Self::IER_RX);
            // SAFETY: self is &'static (guaranteed by Driver::init signature);
            // the UART lives for the entire kernel lifetime.
            trap::register_interrupt_handler(self);
        }
        Ok(())
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                // SAFETY: MMIO region is identity-mapped during driver init; called after Uart::init().
                unsafe { self.write_byte(b'\r') };
            }
            // SAFETY: MMIO region is identity-mapped during driver init.
            unsafe { self.write_byte(b) };
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
            // SAFETY: UART MMIO mapped during init; called from trap handler after boot.
            unsafe { self.write_byte(b'\r') };
        }
        // SAFETY: UART MMIO mapped during init.
        unsafe { self.write_byte(c) };
    }

    fn enable_interrupt(&self) {
        unsafe { self.write(Self::IER, Self::IER_RX) }
    }
}

/// Create and register a UART instance.
pub(crate) fn register(name: &'static str, base: usize, interrupt: u32) -> &'static Uart {
    // SAFETY: allocator is initialized during boot; instance is leaked for permanent lifetime.
    let uart = alloc::boxed::Box::new(Uart::new(base, interrupt));
    let uart_ref = alloc::boxed::Box::leak(uart);
    super::hub::register::<Uart>(uart_ref, name);
    uart_ref
}
