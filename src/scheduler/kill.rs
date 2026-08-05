// 终止其它任务（scheduler 子模块）
//
// kill(id) 是"他杀"域：从任一调度队列（就绪/睡眠/僵尸）移出目标并回收；
// 若有人在等它（wait_pid == id 阻塞中）→ 唤醒父并告知被 kill（wait 返回
// None）。杀当前任务必须走 exit()（kill 返回 IsCurrent）。

use crate::info;

use super::schedule::reclaim_one;
use super::sleep::{resume_after_wait, wake_task};
use super::task::{TASK_TABLE, WaitResult};

/// 终止目标任务失败的原因（错误码语义即行为：变体名直接对应处置）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KillError {
    /// 没有该 id 的任务（就绪/睡眠/僵尸队列中均不存在）
    NotFound,
    /// 目标是当前任务——杀自己必须走 `exit()`
    IsCurrent,
}

/// 终止指定任务（他杀）。目标在就绪/睡眠/僵尸任一队列 → 移出并回收
/// （栈 + 独占地址空间）；若有任务正在 `wait(id)` 阻塞 → 唤醒它并告知
/// 目标被 kill（wait 返回 None）。
///
/// # Errors
/// - 目标在 CURRENT（运行中）→ [`KillError::IsCurrent`]：自己只能 `exit()`
/// - 目标不存在 → [`KillError::NotFound`]
///
/// # 单 hart 保证
/// 「唤醒等待者」的查找（`take_sleeper_waiting`）只覆盖已 park 进睡眠队列
/// 的父；`wait()` 的①收尸/②判活/③置 Wait 在同一关中断区间，故单 hart 下
/// 不存在「父未 park 且 kill 先回收目标」的窗口（kill 执行时 CURRENT 恒为
/// 调用者自己）。多核（task.rs 的 per-hart CURRENT TODO 落地）后需在此处
/// 补查其他 hart CURRENT 上 `wait_pid == id` 的等待者。
pub fn kill(id: usize) -> Result<(), KillError> {
    let mut table = TASK_TABLE.lock();
    // 目标是当前任务：kill 语义是他杀，杀自己必须走 exit()
    if table.current().is_some_and(|c| c.id == id) {
        return Err(KillError::IsCurrent);
    }
    // 从任一队列移出目标（ready → sleep → zombie）
    let Some(target) = table.take(id) else {
        return Err(KillError::NotFound);
    };
    // 有人在等它（wait_pid == id 阻塞中）→ 唤醒父并告知被 kill
    if let Some(mut parent) = table.take_sleeper_waiting(id) {
        parent.wait_result = WaitResult::Killed;
        parent.resume_sepc = resume_after_wait as *const () as usize;
        parent.wait_pid = None;
        wake_task(&mut parent);
        table.push_ready(parent);
    }
    drop(table);
    reclaim_one(target);
    info!("killed task {id:#x}");
    Ok(())
}
