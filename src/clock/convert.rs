// 时间换算 — Duration ↔ ticks ↔ 微秒（全内核唯一换算处）
//
// 消灭原 log/clock.rs 与 scheduler/sleep.rs 的两份重复换算：
// 频率统一取自 source::frequency()（未注册 panic）。

use core::time::Duration;

use super::source;

/// ticks → Duration（饱和，余数换算用 u128 中间量防溢出）。
pub fn ticks_to_duration(ticks: u64) -> Duration {
    let freq = source::frequency();
    let secs = ticks / freq;
    let sub = ticks % freq;
    // 亚秒部分：sub < freq，`sub * 1e9` 在 freq 接近 1e9 时溢出 u64。
    let nanos = ((sub as u128 * 1_000_000_000) / freq as u128) as u32;
    Duration::new(secs, nanos)
}

/// Duration → ticks（saturating，超大时长不 panic，只到 u64 能表达的极限）。
///
/// 公式：`ticks = secs × freq + subsec_nanos × freq / 1e9`。
pub fn duration_to_ticks(d: Duration) -> u64 {
    let freq = source::frequency();
    d.as_secs().saturating_mul(freq).saturating_add(
        (d.subsec_nanos() as u64)
            .saturating_mul(freq)
            .saturating_div(1_000_000_000),
    )
}

/// ticks → 微秒总数（分解式防 `ticks * 1e6` 溢出；余数刻度按比例换算）。
pub fn ticks_to_usecs(ticks: u64) -> u64 {
    let freq = source::frequency().max(1);
    ticks / freq * 1_000_000 + (ticks % freq) * 1_000_000 / freq
}
