// 时钟子系统 — 内核单调时间服务
//
// 全内核唯一的时间入口：时间源（source.rs）、换算（convert.rs）、
// tick 管理（tick.rs）、软定时器（timer.rs）全部收敛于此。
// 依赖方向单向：scheduler → clock、clock → hal::get_internal（重装能力）、
// clint → clock::TICK_HZ（策略注入）；clock 不依赖 scheduler。
// 既有 API 保持：hal::InternalInterrupt 原样不动，clock 消费它而非改它。

mod convert;
mod delay;
mod source;
mod tick;
mod timer;

// Clock / CsrClock 为预留 API（当前无调用方），allow 保留导出
// （log/mod.rs 预留 API 同款先例）
#[allow(unused_imports)]
pub use convert::{duration_to_ticks, ticks_to_duration, ticks_to_usecs};
pub use delay::delay;
#[allow(unused_imports)]
pub use source::{frequency, init, now, Clock, CsrClock, CSR_CLOCK};
#[allow(unused_imports)]
pub use tick::{jiffies, on_timer, start, TICK_HZ};
pub use timer::{cancel, register, register_periodic, TimerId};
