// Goldfish RTC 驱动 — QEMU virt 的墙上时钟（google,goldfish-rtc）
//
// QEMU virt 的 RTC 设备 @ 0x10000000，寄存器布局（goldfish RTC 规范，
// 时间为**纳秒**精度，自 epoch 计数）：
//   +0x00 TIME_LO — 时间低 32 位（只读）
//   +0x04 TIME_HI — 时间高 32 位（只读）
// 读 LO/HI 后重读 LO 校验（低 32 位回绕窗口），取两读中较大者。
// 只读设备，无需中断。probe 时注册到 crate::hal::rtc 契约层
// （driver/uart 实现 crate::hal::uart::Uart 同构）。

use crate::driver::device::Device;
use crate::driver::traits::{Driver, DriverError};

/// Goldfish RTC — 提供墙上时间（epoch 秒）能力。
#[derive(Debug)]
pub struct GoldfishRtc {
    base: usize,
}

// SAFETY: single-hart kernel; MMIO base address is valid for the lifetime of the system.
unsafe impl Sync for GoldfishRtc {}

impl GoldfishRtc {
    /// 读取 epoch 纳秒数（TIME_LO@+0x00 / TIME_HI@+0x04 + 重读 LO 防回绕窗口）。
    fn read_epoch(&self) -> u64 {
        // SAFETY: probe 已 map_mmio 映射该区域，base 有效。
        unsafe {
            let lo1 = (self.base as *const u32).read_volatile() as u64;
            let hi = (self.base as *const u32).add(1).read_volatile() as u64;
            let lo2 = (self.base as *const u32).read_volatile() as u64;
            (hi << 32) | lo1.max(lo2)
        }
    }
}

impl crate::hal::rtc::Realtime for GoldfishRtc {
    fn epoch_secs(&self) -> u64 {
        // goldfish RTC 时间为纳秒，契约层以秒为单位（截断余数）
        self.read_epoch() / 1_000_000_000
    }
}

/// Goldfish RTC 驱动。
pub struct GoldfishRtcDriver;

impl Driver for GoldfishRtcDriver {
    fn name(&self) -> &'static str {
        "goldfish-rtc"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["google,goldfish-rtc"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // MMIO 映射（driver::map_mmio：取整 + 内核空间映射）
        unsafe { crate::driver::map_mmio(dev) }?;

        // 构造实例 + 挂载（Linux dev_set_drvdata 语义）
        let rtc = alloc::boxed::Box::leak(alloc::boxed::Box::new(GoldfishRtc {
            base: dev.base.as_usize(),
        }));
        dev.set_instance(rtc);

        // 注册到硬件能力契约层（hal::rtc::epoch_secs 查询入口）
        crate::hal::rtc::register(rtc);
        Ok(())
    }
}

/// 驱动静态实例（rtc::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &GoldfishRtcDriver;
