// 时钟源 — 单调时间读取
//
// boot 最早注册一次（clock::init），之后读时刻仅一次虚调用（读 `time` CSR）。
// 存储用 OnceLock（写一次读多次）；未注册时 now/frequency panic——
// init 是 boot 铁律，与 platform::get 同款语义。
// 本文件由原 log/clock.rs 升级而来：契约 Clock 与 CsrClock 平移，
// set_clock_source 改名 init（与 platform::init / trap::init 命名一致）。

use crate::lock::OnceLock;

/// 单调时钟源 — 内核时间基准（如 mtime 刻度）。
///
/// boot 阶段注册一次，之后读时刻仅一次虚调用（如读 `time` CSR）。
pub trait Clock: Sync {
    /// 返回当前时刻的时钟计数值（如 mtime/cycle）。
    fn now(&self) -> u64;
}

/// 直接读 `time` CSR 的时钟源（Sstc 扩展，S-mode 可访问）。
///
/// boot 最早即可用，无需任何驱动实例——`clock::init` 在驱动初始化前注册，
/// 让早期启动日志（console/驱动就绪前）也带真实时间戳。
pub struct CsrClock;

// SAFETY: 无内部状态，读 CSR 无副作用。
unsafe impl Sync for CsrClock {}

impl Clock for CsrClock {
    fn now(&self) -> u64 {
        let t: u64;
        // SAFETY: `time` CSR 是 Sstc 扩展的只读时间计数器，S-mode 可读。
        unsafe { core::arch::asm!("csrr {}, time", out(reg) t) };
        t
    }
}

/// 全局默认时钟源实例。
pub static CSR_CLOCK: CsrClock = CsrClock;

/// 时钟源（时钟对象 + 频率），init 时注册一次。
struct Source {
    clock: &'static dyn Clock,
    freq: u64,
}

/// 时钟源存储（写一次读多次）。
static SOURCE: OnceLock<Source> = OnceLock::new();

/// 注册时钟源（main() 第一行调用一次，boot 最早阶段）。
///
/// `clock` 返回当前 mtime 值，`freq` 为定时器频率 (Hz)。
pub fn init(clock: &'static dyn Clock, freq: u64) {
    SOURCE.set(Source { clock, freq }).ok();
}

/// 当前时刻（mtime 刻度）。
///
/// # Panics
///
/// 若 `init()` 未被调用（boot 铁律，未注册即 bug）。
#[inline]
pub fn now() -> u64 {
    SOURCE.get().expect("clock::init() not called").clock.now()
}

/// 定时器频率（Hz）。
///
/// # Panics
///
/// 若 `init()` 未被调用。
#[inline]
pub fn frequency() -> u64 {
    SOURCE.get().expect("clock::init() not called").freq
}
