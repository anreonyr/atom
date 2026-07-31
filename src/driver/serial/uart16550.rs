// NS16550A 串口驱动 — serial/ 角色目录
//
// Uart16550Driver 匹配 "ns16550a" 设备；probe 构造 Uart16550 实例，
// 挂载到设备 instance 上，完成硬件初始化与中断路由。
//
// 中断路由依赖 PLIC 已 probe（bus::find::<Plic>），未就绪时返回
// DriverError::Deferred，bus 自动延后重试。

use core::fmt;

use crate::driver::bus;
use crate::driver::controller::plic::Plic;
use crate::driver::device::Device;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::{ExternalInterrupt, InterruptHandler, Mmio};
use crate::memory::allocator::page;
use crate::trap;

/// 16550 UART 实例 — MMIO 操作 + 输出 + 中断处理。
#[derive(Debug)]
pub struct Uart16550 {
    base: *mut u8,
    interrupt: u32,
}

// SAFETY: single-hart kernel; MMIO base pointer is valid for the lifetime of the system.
unsafe impl Send for Uart16550 {}
unsafe impl Sync for Uart16550 {}

impl Uart16550 {
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

    // ── LCR / FCR / IER / LSR 位 ──────────────────────────
    const LCR_DLAB: u8 = 0x80;
    const FCR_ENABLE: u8 = 0x01;
    const FCR_CLR_RX: u8 = 0x02;
    const FCR_CLR_TX: u8 = 0x04;
    const FCR_TRIG_14: u8 = 0xC0;
    const IER_RX: u8 = 0x01;
    const LSR_THRE: u8 = 0x20;

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

    /// 硬件初始化：波特率 / FIFO / 8N1。
    fn init_hw(&self) -> Result<(), DriverError> {
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
        Ok(())
    }
}

impl Mmio for Uart16550 {
    type T = u8;

    fn base(&self) -> *mut u8 {
        self.base
    }
}

impl fmt::Write for Uart16550 {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                // SAFETY: MMIO region is identity-mapped during driver init; called after probe.
                unsafe { self.write_byte(b'\r') };
            }
            // SAFETY: MMIO region is identity-mapped during driver init.
            unsafe { self.write_byte(b) };
        }
        Ok(())
    }
}

impl InterruptHandler for Uart16550 {
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

/// 16550 驱动。
pub struct Uart16550Driver;

impl Driver for Uart16550Driver {
    fn name(&self) -> &'static str {
        "uart16550"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["ns16550a"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // 依赖检查前置：PLIC 未 probe 时直接返回 Deferred，不产生任何副作用。
        // deferred 重试会再次调用本 probe，副作用（映射/构造/注册）必须发生在
        // 依赖就绪之后，保证幂等（Linux probe 失败回滚语义）。
        let plic = bus::find::<Plic>().ok_or(DriverError::Deferred)?;
        let irq = dev.interrupt.unwrap_or(10);

        // MMIO 映射（自含，不再有中央 map_devices）
        unsafe { crate::memory::map_device(dev.base.as_usize(), dev.size, page::allocator()) }
            .map_err(|_| DriverError::MapFailed(dev.compatible))?;

        // 构造实例 + 挂载到设备（Linux dev_set_drvdata 语义）
        let uart = alloc::boxed::Box::leak(alloc::boxed::Box::new(Uart16550::new(
            dev.base.as_usize(),
            irq,
        )));
        dev.set_instance(uart);

        // 硬件初始化（波特率 / FIFO / 8N1）
        uart.init_hw()?;

        // 中断路由
        plic.set_priority(irq, 1);
        plic.enable(irq);
        uart.enable_interrupt();
        trap::register_interrupt_handler(uart);

        Ok(())
    }
}

/// 驱动静态实例（serial::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &Uart16550Driver;
