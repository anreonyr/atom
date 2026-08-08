// 墙上时钟硬件能力契约 — RTC 设备能力 trait + 注册表
//
// 纯硬件能力（零依赖）+ 注册表（与 hal::interrupt 的 register/get 同构：
// OnceLock 写一次读多次）。驱动（driver/rtc/）实现本 trait，probe 时经
// `register()` 注册；消费方经 `epoch_secs()` 查询（未注册返回 None）。
//
// 对应 Linux：RTC 设备驱动的注册表面；clock 子系统（单调时间）与墙上
// 时间（RTC）分离——clock 不涉及 Realtime。

use crate::lock::OnceLock;

/// 墙上时钟源 — 自 Unix epoch 的真实时间（硬件能力契约）。
pub trait Realtime: Sync {
    /// 当前真实时间（自 epoch 起的秒数；粒度由实现决定）。
    fn epoch_secs(&self) -> u64;
}

static REALTIME: OnceLock<&'static dyn Realtime> = OnceLock::new();

/// 注册墙上时钟源 — RTC 驱动 probe 时调用一次。
pub fn register(r: &'static dyn Realtime) {
    if REALTIME.set(r).is_err() {
        crate::warn!("realtime already registered (rtc::register called more than once)");
    }
}

/// 当前墙上时间（自 epoch 起的秒数；未注册返回 None）。
pub fn epoch_secs() -> Option<u64> {
    REALTIME.get().map(|r| r.epoch_secs())
}
