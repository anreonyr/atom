// SiFive UART 串口驱动 — serial/ 角色目录
//
// SifiveUartDriver 匹配 "sifive,uart0" 设备（QEMU sifive_u 提供两个此型号 UART）。
// probe 构造 SifiveUart 实例，挂载到设备 instance，完成硬件初始化与中断路由，
// 并注册到 crate::uart 注册表（console / devfs 枚举用）。
//
// 寄存器布局（32 位，与 NS16550A 不同）：
//   TXDATA(0x00) / RXDATA(0x04) / TXCTRL(0x08) / RXCTRL(0x0C) / IE(0x10) / IP(0x14) / DIV(0x18)
//   TXDATA bit31 = TX FIFO full；RXDATA bit31 = RX FIFO empty
//
// 驱动只实现 crate::uart::Uart 能力；File / fmt::Write / InterruptHandler
// 三个视图由 src/uart.rs 的 blanket 适配提供——本文件不出现 File 类型。
//
// 中断路由依赖 PLIC 已 probe（hub::find::<Plic>），未就绪时返回
// DriverError::Deferred，hub 自动延后重试。

use crate::driver::controller::plic::Plic;
use crate::driver::device::Device;
use crate::driver::hub;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::ExternalInterrupt;
use crate::memory::addr::PhysAddr;
use crate::uart::Uart;
use crate::uart;

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
    // IP(0x14)：中断挂起位——RXWM 挂起判定已上移服务层（InputHandler 用
    // read_byte 轮询 RXDATA 搬字符），常量不再需要，寄存器布局见头注释。
    // DIV(0x18)：波特率分频。QEMU 模拟的 sifive uart 不依赖波特率，保持默认。

    // ── 标志位 ─────────────────────────────────────────────
    const TXDATA_FULL: u32 = 1 << 31;
    const RXDATA_EMPTY: u32 = 1 << 31;
    const CTRL_ENABLE: u32 = 0x1;
    const IE_RXWM: u32 = 0x2;

    /// DTB 缺 `interrupts` 属性时的默认中断号（QEMU sifive_u 的 uart0 为 4）。
    const DEFAULT_INTERRUPT: u32 = 4;

    pub const fn new(base: PhysAddr, interrupt: u32) -> Self {
        Self {
            base: base.as_usize() as *mut u8,
            interrupt,
        }
    }

    /// 读取寄存器 — volatile 内联（避免与 File::read 同名歧义）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    #[inline]
    unsafe fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { (self.base.add(offset) as *const u32).read_volatile() }
    }

    /// 写入寄存器 — volatile 内联（避免与 File::write 同名歧义）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    #[inline]
    unsafe fn write_reg(&self, offset: usize, val: u32) {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { (self.base.add(offset) as *mut u32).write_volatile(val) }
    }

    /// 硬件初始化：使能 TX/RX 通道。
    fn init(&self) -> Result<(), DriverError> {
        unsafe {
            self.write_reg(Self::TXCTRL, Self::CTRL_ENABLE);
            self.write_reg(Self::RXCTRL, Self::CTRL_ENABLE);
        }
        Ok(())
    }
}

impl Uart for SifiveUart {
    /// 写入单字节（锁外，轮询 TX FIFO full）。panic handler 使用。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    unsafe fn write_byte(&self, c: u8) {
        while unsafe { self.read_reg(Self::TXDATA) } & Self::TXDATA_FULL != 0 {}
        unsafe { self.write_reg(Self::TXDATA, c as u32) }
    }

    /// 非阻塞读取单字节（RXDATA bit31 置位 = RX FIFO empty）。
    fn read_byte(&self) -> Option<u8> {
        let rxd = unsafe { self.read_reg(Self::RXDATA) };
        if rxd & Self::RXDATA_EMPTY != 0 {
            return None;
        }
        Some((rxd & 0xFF) as u8)
    }

    fn interrupt_number(&self) -> u32 {
        self.interrupt
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
        let plic = hub::find::<Plic>().ok_or(DriverError::Deferred)?;
        let irq = dev.interrupt.unwrap_or(SifiveUart::DEFAULT_INTERRUPT);

        // MMIO 映射（自含）
        // MMIO 映射（driver::map_mmio：取整 + 内核空间映射）
        unsafe { crate::driver::map_mmio(dev) }?;

        // 构造实例 + 挂载到设备（Linux dev_set_drvdata 语义）
        let uart = alloc::boxed::Box::leak(alloc::boxed::Box::new(SifiveUart::new(dev.base, irq)));
        dev.set_instance(uart);

        // 硬件初始化（使能 TX/RX）
        uart.init()?;

        // 注册到 uart 注册表（console / devfs 枚举用；双视图在注册表层构造）
        uart::register(uart);

        // 中断路由（handler 注册已并入 uart::register 三联动）
        plic.set_priority(irq, 1);
        plic.enable(irq);
        uart.enable_interrupt();

        Ok(())
    }
}

/// 驱动静态实例（serial::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &SifiveUartDriver;
