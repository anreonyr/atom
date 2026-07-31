// SiFive UART 串口驱动 — serial/ 角色目录
//
// SifiveUartDriver 匹配 "sifive,uart0" 设备（QEMU sifive_u 提供两个此型号 UART）。
// probe 构造 SifiveUart 实例，挂载到设备 instance，完成硬件初始化与中断路由，
// 并注册到 serial 目录的 UART 表（console / devfs 枚举用）。
//
// 寄存器布局（32 位，与 NS16550A 不同）：
//   TXDATA(0x00) / RXDATA(0x04) / TXCTRL(0x08) / RXCTRL(0x0C) / IE(0x10) / IP(0x14) / DIV(0x18)
//   TXDATA bit31 = TX FIFO full；RXDATA bit31 = RX FIFO empty
//
// 中断路由依赖 PLIC 已 probe（bus::find::<Plic>），未就绪时返回
// DriverError::Deferred，bus 自动延后重试。

use core::fmt;

use crate::driver::bus;
use crate::driver::controller::plic::Plic;
use crate::driver::device::Device;
use crate::driver::traits::{Driver, DriverError};
use crate::filesystem::traits::File;
use crate::hal::{ExternalInterrupt, InterruptHandler, Mmio};
use crate::memory::allocator::page;
use crate::trap;

/// SiFive UART 实例 — MMIO 操作 + 输出 + 中断处理。
#[derive(Debug)]
pub struct SifiveUart {
    base: *mut u8,
    interrupt: u32,
}

// SAFETY: single-hart kernel; MMIO base pointer is valid for the lifetime of the system.
unsafe impl Send for SifiveUart {}
unsafe impl Sync for SifiveUart {}

impl SifiveUart {
    // ── 寄存器偏移 ─────────────────────────────────────────
    const TXDATA: usize = 0x00;
    const RXDATA: usize = 0x04;
    const TXCTRL: usize = 0x08;
    const RXCTRL: usize = 0x0C;
    const IE: usize = 0x10;
    const IP: usize = 0x14;
    // DIV(0x18)：波特率分频。QEMU 模拟的 sifive uart 不依赖波特率，保持默认。

    // ── 标志位 ─────────────────────────────────────────────
    const TXDATA_FULL: u32 = 1 << 31;
    const RXDATA_EMPTY: u32 = 1 << 31;
    const CTRL_ENABLE: u32 = 0x1;
    const IE_RXWM: u32 = 0x2;

    pub const fn new(base: usize, interrupt: u32) -> Self {
        Self {
            base: base as *mut u8,
            interrupt,
        }
    }

    /// 读取寄存器 — Mmio 封装（避免与 File::read 同名歧义）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    #[inline]
    unsafe fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { <Self as Mmio>::read(self, offset) }
    }

    /// 写入寄存器 — Mmio 封装（避免与 File::write 同名歧义）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    #[inline]
    unsafe fn write_reg(&self, offset: usize, val: u32) {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { <Self as Mmio>::write(self, offset, val) }
    }

    /// 写入单字节（锁外，轮询 TX FIFO full）。panic handler 使用。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    pub(crate) unsafe fn write_byte(&self, c: u8) {
        while unsafe { self.read_reg(Self::TXDATA) } & Self::TXDATA_FULL != 0 {}
        unsafe { self.write_reg(Self::TXDATA, c as u32) }
    }

    /// 硬件初始化：使能 TX/RX 通道。
    fn init_hw(&self) -> Result<(), DriverError> {
        unsafe {
            self.write_reg(Self::TXCTRL, Self::CTRL_ENABLE);
            self.write_reg(Self::RXCTRL, Self::CTRL_ENABLE);
        }
        Ok(())
    }
}

impl Mmio for SifiveUart {
    type T = u32;

    fn base(&self) -> *mut u8 {
        self.base
    }
}

impl File for SifiveUart {
    /// 从 UART 轮询读取一个字节（阻塞等待数据就绪）。
    fn read(&self, _offset: usize, buf: &mut [u8]) -> crate::filesystem::traits::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // 轮询等待数据就绪（RXDATA bit31 清除）
        let mut rxd = unsafe { self.read_reg(Self::RXDATA) };
        while rxd & Self::RXDATA_EMPTY != 0 {
            core::hint::spin_loop();
            rxd = unsafe { self.read_reg(Self::RXDATA) };
        }
        buf[0] = (rxd & 0xFF) as u8;
        Ok(1)
    }

    /// 向 UART 写入字节（`\n` 自动转换为 `\r\n`）。
    fn write(&self, _offset: usize, buf: &[u8]) -> crate::filesystem::traits::Result<usize> {
        for &b in buf {
            if b == b'\n' {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.write_byte(b'\r') };
            }
            // SAFETY: MMIO region is identity-mapped during driver init.
            unsafe { self.write_byte(b) };
        }
        Ok(buf.len())
    }
}

impl fmt::Write for SifiveUart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                // SAFETY: MMIO region is identity-mapped during driver init.
                unsafe { self.write_byte(b'\r') };
            }
            // SAFETY: MMIO region is identity-mapped during driver init.
            unsafe { self.write_byte(b) };
        }
        Ok(())
    }
}

impl InterruptHandler for SifiveUart {
    fn interrupt_number(&self) -> u32 {
        self.interrupt
    }

    fn handle_interrupt(&self) {
        // RXWM 中断：读取并回显
        if unsafe { self.read_reg(Self::IP) } & Self::IE_RXWM != 0 {
            let rxd = unsafe { self.read_reg(Self::RXDATA) };
            if rxd & Self::RXDATA_EMPTY == 0 {
                let c = (rxd & 0xFF) as u8;
                if c == b'\r' {
                    // SAFETY: UART MMIO mapped during init.
                    unsafe { self.write_byte(b'\r') };
                }
                // SAFETY: UART MMIO mapped during init.
                unsafe { self.write_byte(c) };
            }
        }
    }

    fn enable_interrupt(&self) {
        unsafe { self.write_reg(Self::IE, Self::IE_RXWM) }
    }
}

/// SiFive UART 驱动。
pub struct SifiveUartDriver;

impl Driver for SifiveUartDriver {
    fn name(&self) -> &'static str {
        "sifive-uart"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["sifive,uart0"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // 依赖检查前置：PLIC 未 probe 时直接返回 Deferred，不产生任何副作用。
        let plic = bus::find::<Plic>().ok_or(DriverError::Deferred)?;
        let irq = dev.interrupt.unwrap_or(4);

        // MMIO 映射（自含）
        unsafe { crate::memory::map_device(dev.base.as_usize(), dev.size, page::allocator()) }
            .map_err(|_| DriverError::MapFailed(dev.compatible))?;

        // 构造实例 + 挂载到设备（Linux dev_set_drvdata 语义）
        let uart = alloc::boxed::Box::leak(alloc::boxed::Box::new(SifiveUart::new(
            dev.base.as_usize(),
            irq,
        )));
        dev.set_instance(uart);

        // 硬件初始化（使能 TX/RX）
        uart.init_hw()?;

        // 注册到 serial 目录（console / devfs 枚举用）
        crate::driver::serial::register(
            uart as &'static dyn File,
            uart as &'static dyn core::fmt::Write,
        );

        // 中断路由
        plic.set_priority(irq, 1);
        plic.enable(irq);
        uart.enable_interrupt();
        trap::register_interrupt_handler(uart);

        Ok(())
    }
}

/// 驱动静态实例（serial::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &SifiveUartDriver;
