// 时钟源 — 日志时间戳来源
//
// boot 阶段注册一次（set_clock_source），之后每条日志仅一次虚调用
// （如读 `time` CSR），避免日志路径做设备表查找。
// 换算只发生一次（read_time）：mtime tick → 微秒总数，显示格式在消费处拆分。
// 时钟源存储用 OnceLock（写一次读多次）；早期未注册时 read_time 返回 None。

use core::fmt::Write as _;

use crate::lock::OnceLock;

use super::buf::Buf;
use super::record::Timestamp;

/// 单调时钟源 — 内核日志的时间戳来源。
///
/// boot 阶段注册一次，之后每条日志仅一次虚调用（如读 `time` CSR），
/// 避免日志路径做设备表查找。
pub trait Clock: Sync {
    /// 返回当前时刻的时钟计数值（如 mtime/cycle）。
    fn now(&self) -> u64;
}

/// 直接读 `time` CSR 的时钟源（Sstc 扩展，S-mode 可访问）。
///
/// boot 最早即可用，无需任何驱动实例——`set_clock_source` 在驱动初始化前注册，
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
struct ClockSource {
    clock: &'static dyn Clock,
    freq: u64,
}

/// 时钟源存储（写一次读多次）。
static CLOCK: OnceLock<ClockSource> = OnceLock::new();

/// 注册时钟源（应在 init 阶段调用一次）。
///
/// `clock` 返回当前 mtime 值，`freq` 为定时器频率 (Hz)。
pub fn set_clock_source(clock: &'static dyn Clock, freq: u64) {
    CLOCK.set(ClockSource { clock, freq }).ok();
}

/// 读取当前时间戳——未注册时钟源时返回 None。
pub(super) fn read_time() -> Option<Timestamp> {
    CLOCK.get().map(|src| {
        let freq = src.freq.max(1);
        let mt = src.clock.now();
        // 微秒总数（分解式防 `mt * 1e6` 溢出）；余数刻度换算见下注释
        let usec_total = mt / freq * 1_000_000 + (mt % freq) * 1_000_000 / freq;
        Timestamp::new(usec_total)
    })
}

/// 格式化时间戳显示串（未注册时钟时为 `?.??????`）。
pub(super) fn fmt_time(ts: Option<Timestamp>) -> Buf<24> {
    let mut t = Buf::new();
    match ts {
        Some(ts) => {
            let _ = write!(t, "{}.{:06}", ts.sec(), ts.usec());
        }
        None => {
            let _ = t.write_str("?.??????");
        }
    }
    t
}
