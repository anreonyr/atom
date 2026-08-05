// tick 管理 — 内核调度/定时粒度
//
// TICK_HZ = 100（10ms）。时钟中断经 trap 编排进入 on_timer：
// 重装（经 hal::InternalInterrupt::handle_timer，间隔由 Clint 依 TICK_HZ
// 决定——驱动→clock 单向策略注入）→ jiffies++ → 软定时器到期分发。
// 不碰 TrapFrame/调度：调度由 trap 在 on_timer 之后编排
// （scheduler 依赖 clock，clock 不依赖 scheduler）。

use core::sync::atomic::{AtomicU64, Ordering};

use crate::hal::interrupt::get_internal;

/// tick 频率（Hz）— 内核调度/定时粒度（100Hz = 10ms）。
pub const TICK_HZ: u64 = 100;

/// jiffies 计数 — 自 boot 的 tick 数（诊断用）。
static JIFFIES: AtomicU64 = AtomicU64::new(0);

/// 自 boot 起的 tick 数（诊断/日志用）。
pub fn jiffies() -> u64 {
    JIFFIES.load(Ordering::Relaxed)
}

/// 首次装载定时中断 — init::run() 返回后、sie 使能前调用一次。
///
/// 未注册内部中断控制器时 warn + no-op（防御；正常流程恒已注册）。
pub fn start() {
    match get_internal() {
        Some(ii) => {
            // wrapping_add：mtime 回绕时 next 仍是未来的小值（与 handle_timer 同款）
            let next = ii.read().wrapping_add(crate::clock::frequency() / TICK_HZ);
            ii.next(next);
        }
        None => crate::warn!("clock::tick::start: internal controller not registered"),
    }
}

/// tick 处理 — trap STI 入口调用（无参、不碰 TrapFrame/调度）。
///
/// 重装（经 InternalInterrupt::handle_timer）→ jiffies++ → 软定时器到期分发。
/// 返回 false = 内部中断控制器未注册（trap 据此降级：不调度）。
pub fn on_timer() -> bool {
    let Some(ii) = get_internal() else {
        crate::warn!("timer interrupt before internal controller registered");
        return false;
    };
    ii.handle_timer();
    JIFFIES.fetch_add(1, Ordering::Relaxed);
    // 软定时器到期分发（回调中断上下文执行：先取列表、锁外分发）
    super::timer::run(crate::clock::now());
    true
}
