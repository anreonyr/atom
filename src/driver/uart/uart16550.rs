// NS16550A 串口驱动 — serial/ 角色目录
//
// Uart16550Driver 匹配 "ns16550a" 设备；probe 构造 Uart16550 实例，
// 挂载到设备 instance 上，完成硬件初始化与中断路由。
//
// 驱动只实现 crate::hal::byte_channel::ByteChannel 能力（寄存器操作 + 非阻塞读 + 中断处理）；
// File（VFS）/ fmt::Write（console）/ InterruptHandler（中断路由）三个视图
// 由 src/uart.rs 的 blanket 适配提供——本文件不出现 File 类型。
//
// 中断路由依赖 PLIC 已 probe（hub::find::<Plic>），未就绪时返回
// DriverError::Deferred，hub 自动延后重试。

use alloc::boxed::Box;

use crate::driver::controller::plic::Plic;
use crate::driver::device::Device;
use crate::driver::hub;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::ExternalInterrupt;
use crate::hal::byte_channel::ByteChannel;
use crate::io::console;
use crate::memory::addr::PhysAddr;

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

    /// DTB 缺 `interrupts` 属性时的默认中断号（QEMU virt 的 ns16550a 为 10）。
    const DEFAULT_INTERRUPT: u32 = 10;

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
    unsafe fn read_reg(&self, offset: usize) -> u8 {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { (self.base.add(offset) as *const u8).read_volatile() }
    }

    /// 写入寄存器 — volatile 内联（避免与 File::write 同名歧义）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    #[inline]
    unsafe fn write_reg(&self, offset: usize, val: u8) {
        // SAFETY: 由调用方保证 MMIO 已映射。
        unsafe { self.base.add(offset).write_volatile(val) }
    }

    /// 硬件初始化：波特率 / FIFO / 8N1。
    fn init(&self) -> Result<(), DriverError> {
        let divisor = (Self::CLOCK / (16 * Self::BAUD)) as u16;
        unsafe {
            self.write_reg(Self::IER, 0x00);
            self.write_reg(
                Self::FCR,
                Self::FCR_ENABLE | Self::FCR_CLR_RX | Self::FCR_CLR_TX | Self::FCR_TRIG_14,
            );
            self.write_reg(Self::LCR, Self::LCR_DLAB);
            self.write_reg(Self::DLL, (divisor & 0xFF) as u8);
            self.write_reg(Self::DLM, ((divisor >> 8) & 0xFF) as u8);
            self.write_reg(Self::LCR, 0x03);
        }
        Ok(())
    }
}

impl ByteChannel for Uart16550 {
    /// 写入单字节（锁外，轮询 THRE）。panic handler 使用。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    unsafe fn write_byte(&self, c: u8) {
        while unsafe { self.read_reg(Self::LSR) } & Self::LSR_THRE == 0 {}
        unsafe { self.write_reg(Self::THR, c) }
    }

    /// 非阻塞读取单字节（LSR bit 0: Data Ready）。
    fn read_byte(&self) -> Option<u8> {
        if unsafe { self.read_reg(Self::LSR) } & 0x01 == 0 {
            return None;
        }
        Some(unsafe { self.read_reg(Self::RBR) })
    }

    fn interrupt_number(&self) -> u32 {
        self.interrupt
    }

    fn enable_interrupt(&self) {
        unsafe { self.write_reg(Self::IER, Self::IER_RX) }
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
        let plic = hub::find::<Plic>().ok_or(DriverError::Deferred)?;
        let irq = dev.interrupt.unwrap_or(Uart16550::DEFAULT_INTERRUPT);

        // MMIO 映射（driver::map_mmio：取整 + 内核空间映射）
        unsafe { crate::driver::map_mmio(dev) }?;

        // 构造实例 + 挂载到设备（Linux dev_set_drvdata 语义）
        let uart = Box::leak(Box::new(Uart16550::new(dev.base, irq)));
        dev.set_instance(uart);

        // 硬件初始化（波特率 / FIFO / 8N1）
        uart.init()?;

        // 注册到终端核心（console / devfs 枚举用；Console/InputHandler/RawFile 在核心构造）
        console::register(uart);

        // 中断路由（handler 注册已并入 console::register）
        plic.set_priority(irq, 1);
        plic.enable(irq);
        uart.enable_interrupt();

        Ok(())
    }
}

/// 驱动静态实例（serial::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &Uart16550Driver;
