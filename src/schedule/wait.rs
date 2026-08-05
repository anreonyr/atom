// 父任务等待子任务（scheduler 子模块）
//
// wait(pid) 是事件阻塞：子退出时 scheduler 经 take_sleeper_waiting 唤醒父
// 并把退出码写进 wait_result（Reap 处置）；子被 kill 时经 kill 路径（kill.rs）
// 唤醒并置 Killed。与 sleep() 同构（wfi 等待 + 恢复段读结果），区别在唤醒
// 事件（子退出）而非时刻（到期）。
//
// 原子性要求：①收尸/②判活/③置 Wait 三步骤必须在**同一关中断区间**内完成
// ——否则父在②与③之间被抢占（重排到就绪队列）时，另一任务可抢先 kill 并
// 回收目标，父随后置 Wait park 将无人再写入 wait_result（永久阻塞）。区间
// 内无抢占（TrapGuard 关中断、单 hart），kill 不可能插入，状态机闭合。

use alloc::boxed::Box;

use crate::{info, lock::TrapGuard};

use super::scheduler::reclaim;
use super::sleep::sleep_wfi;
use super::task::{Pending, TASK_TABLE, Task, WaitResult};

/// 等待指定任务退出并取退出码（对应 Linux `waitpid(pid, &status, 0)` 的简化）。
///
/// 返回：
/// - `Some(code)` — 任务已退出：`exit(code)` / 入口自然返回 `0` / UMode
///   异常终止 `-1`
/// - `None` — 任务不存在，或已被 `kill`（无退出码）
///
/// 约束与 [`super::sleep::sleep`] 相同：不持有任何锁时调用（SIE=0 时 wfi
/// 永不醒）；不得由 boot/空闲任务调用。注意 `wait(own_pid)` 会永久阻塞
/// （无 ECHILD 类防护，与 Linux 语义一致由调用方保证参数合法）。
pub fn wait(pid: usize) -> Option<i32> {
    // 原子区间（关中断）：① 收尸 → ② 判活 → ③ 置 Wait。区间内无抢占，
    // kill 不可能插入——消除「② alive 检查与 ③ 置 Wait 之间被抢占」的
    // 竞态窗口（否则 kill 抢先回收目标后，父置 Wait park 将永久阻塞）。
    let mut reaped: Option<Box<Task>> = None;
    let mut dead = false; // 目标已不存在（从未存在或已被 kill 回收）
    {
        let _ = unsafe { TrapGuard::save() };
        let mut table = TASK_TABLE.lock();
        // ① 收尸：任务已退出且僵尸仍保留 → 直接取退出码并回收
        if let Some(z) = table.take_zombie(pid) {
            reaped = Some(z);
        } else if !table.alive(pid) {
            // ② 目标不存在（且不在僵尸保留）→ 立即返回 None，不阻塞
            dead = true;
        } else if let Some(t) = table.current_mut() {
            // ③ 置 Wait：第一个可能落地的 tick 看到的已是 Wait，否则任务
            // 带着 Rerun 被抢占，wait 静默失效
            t.pending = Pending::Wait(pid);
            t.wait_pid = Some(pid);
        }
    }
    if let Some(z) = reaped {
        // 已收尸的僵尸（Reap 处置时 exit_code 置 None 作标记）不算可收尸——
        // 退出码已被取走，视同任务不存在（等效 ECHILD）。
        let Some(code) = z.exit_code else {
            reclaim(z);
            return None;
        };
        reclaim(z);
        info!("wait: reaped task {pid:#x} code={code}");
        return Some(code);
    }
    if dead {
        return None;
    }

    // 阻塞等结果：wfi + 恢复段循环，直到 Reap/kill 路径写入 wait_result。
    // wfi 是 hint（中断已挂起时直接返回，任务从未被 park）：此时恢复段读
    // 不到结果，保持 Wait 重进循环再等。
    loop {
        sleep_wfi();
        {
            let _ = unsafe { TrapGuard::save() };
            let mut table = TASK_TABLE.lock();
            if let Some(t) = table.current_mut() {
                let r = core::mem::replace(&mut t.wait_result, WaitResult::Pending);
                if r != WaitResult::Pending {
                    t.pending = Pending::Rerun;
                    t.wait_pid = None;
                    drop(table);
                    return match r {
                        WaitResult::Exited(code) => {
                            info!("wait: task {pid:#x} reaped code={code}");
                            Some(code)
                        }
                        WaitResult::Killed => {
                            info!("wait: task {pid:#x} was killed");
                            None
                        }
                        WaitResult::Pending => unreachable!("checked above"),
                    };
                }
            }
        }
    }
}
