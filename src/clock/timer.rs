// 软定时器队列 — 一次性/周期回调（clock 子模块）
//
// tick 驱动：tick::on_timer 内 timer::run(now) 分发到期条目。
// 存储 SpinLock<Vec<TimerEntry>>（与 SLEEP_LIST 同款线性扫描，数量小，
// 不引 BinaryHeap）。到期分发：锁内 partition 到期/未到期（periodic 以
// now + period 重排）→ 释放锁 → 锁外执行回调（回调内可再注册/取消，
// 避免 SpinLock 重入，符合 lockdep 约束）。
//
// 回调约束（中断上下文执行，tick 处理内、调度前）：
//   短小、非阻塞、不得持锁、不得睡眠（scheduler::sleep 依赖 wfi+tick，
//   中断上下文调用会死锁）。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use crate::lock::SpinLock;

use super::{convert, source};

/// 定时器句柄（cancel 用）。0 保留为无效 id（首个注册 id = 1）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimerId(u64);

/// 软定时器条目 — 绝对 mtime 刻度到期时刻 + 可选周期 + 回调。
#[derive(Clone)]
struct TimerEntry {
    id: TimerId,
    deadline: u64,
    period: Option<u64>,
    callback: &'static dyn Fn(),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static TIMERS: SpinLock<Vec<TimerEntry>> = SpinLock::new(Vec::new());

/// 注册一次性定时器：`delay` 后回调一次，返回句柄。
///
/// 回调在中断上下文（tick 处理内）执行——须短小、非阻塞、不得持锁/睡眠；
/// 可在回调内注册/取消其他定时器（分发先取列表、锁外执行）。
pub fn register(delay: Duration, callback: &'static dyn Fn()) -> TimerId {
    let id = TimerId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    TIMERS.lock().push(TimerEntry {
        id,
        deadline: source::now().wrapping_add(convert::duration_to_ticks(delay)),
        period: None,
        callback,
    });
    id
}

/// 注册周期定时器：每 `period` 回调一次（逾期周期不补偿，`now + period` 重排）。
///
/// 回调约束同 [`register`]。
pub fn register_periodic(period: Duration, callback: &'static dyn Fn()) -> TimerId {
    let period_ticks = convert::duration_to_ticks(period);
    let id = TimerId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    TIMERS.lock().push(TimerEntry {
        id,
        deadline: source::now().wrapping_add(period_ticks),
        period: Some(period_ticks),
        callback,
    });
    id
}

/// 取消定时器（未找到/已触发为 no-op）。
///
/// 回调内取消自己无效——正在执行的条目已从队列取出，不再可寻址。
pub fn cancel(id: TimerId) {
    TIMERS.lock().retain(|t| t.id != id);
}

/// 到期分发 — tick::on_timer 调用（中断上下文）。
pub(crate) fn run(now: u64) {
    // 锁内 partition：到期（periodic 以 now + period 重排后）与未到期
    let mut due: Vec<TimerEntry> = Vec::new();
    {
        let mut timers = TIMERS.lock();
        let mut pending: Vec<TimerEntry> = Vec::new();
        for t in timers.drain(..) {
            if t.deadline <= now {
                if let Some(p) = t.period {
                    // 周期：克隆一份以 now + period 重排入 pending，原条目执行回调
                    let mut rep = t.clone();
                    rep.deadline = now.wrapping_add(p);
                    pending.push(rep);
                }
                due.push(t);
            } else {
                pending.push(t);
            }
        }
        *timers = pending;
    }
    // 锁外执行回调：可再注册/取消，避免 SpinLock 重入
    for t in due {
        (t.callback)();
    }
}
