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
//
// wait_sys（U 程序经 syscall 的等待）与 wait 共享同一状态机，差异在唤醒恢复
// 方式：wait 是 SIE=1 就地阻塞（经 park_current 置 Wait + self-IPI 立即 park，
// resume_after_wait 原地恢复）；wait_sys 在 trap 上下文（SIE=0）置 Pending::Wait
// 后返回 Park，由调度器当场 park，子退出/被杀唤醒后 sret **重放 ecall**
// （resume_sepc=0，见 scheduler Reap 与 kill 的 TaskKind 条件化）——重入
// wait_sys 读 wait_result 返回。

use alloc::boxed::Box;

use crate::{info, lock::TrapGuard};

use super::scheduler::reclaim;
use super::sleep::park_current;
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

    // 阻塞等结果：每次迭代重断言 Pending::Wait(pid) 后经 self-IPI park，直到
    // Reap/kill 路径写入 wait_result 唤醒。park 处置（scheduler 的 Wait 分支）
    // 会先查僵尸队列——子若已退出（僵尸保留），当场收尸唤醒，闭合「子先退、
    // 父后 park」的窗口；子存活则入睡眠队列等 Reap/kill 唤醒。
    loop {
        // 重断言：唤醒时 wake_task 已把 pending 复位为 Rerun，重进循环必须先
        // 恢复阻塞意图，否则下次调度按 Rerun 重排而非 park。
        park_current(Pending::Wait(pid), |t| t.wait_pid = Some(pid));
        {
            let _ = unsafe { TrapGuard::save() };
            let mut table = TASK_TABLE.lock();
            if let Some(t) = table.current_mut() {
                let r = core::mem::replace(&mut t.wait_result, WaitResult::Pending);
                if r != WaitResult::Pending {
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

/// syscall 版 wait 的一次调用结果（`envcall::sys_wait` 据此决定 Ret/Reschedule）。
///
/// 三态对应 trap 上下文只能二选一（返回用户或让出调度）的约束：立即可得 →
/// Ret 返回退出码；目标不存在 → Ret -ECHILD；目标存活 → 置 Wait 让出（Park），
/// 唤醒后重放重入。
pub(crate) enum WaitSys {
    /// 退出码已就绪（当场收尸，或 Reap/kill 唤醒重放后从 wait_result 读到）。
    Code(usize),
    /// 目标不存在 / 已被 kill / 已被其他等待者收尸 —— 等效 ECHILD，不阻塞。
    NoTask,
    /// 目标存活：已置 `Pending::Wait(pid)` + resume_sepc=0，调用方返回
    /// `DispatchResult::Reschedule` 让调度器 park。
    Park,
}

/// syscall 版 wait —— 供 `envcall::sys_wait` 在 trap 上下文（SIE=0）调用。
///
/// 与 [`wait`] 同构的原子区间，差异：
/// - 重入优先：等待期间被 Reap（子退出）/kill（子被杀）唤醒后 sret 重放 ecall，
///   重入本函数先从 `wait_result` 读结果（Reap 已把退出码交付于此、子已收尸）；
/// - 置 Wait 时 `resume_sepc = 0`（UMode 保持 ecall 地址，sret 重放）；`wait`
///   的就地恢复依赖 Reap/kill 写 `resume_after_wait`，二者不冲突（scheduler Reap
///   与 kill 按 TaskKind 区分恢复点）。
pub(crate) fn wait_sys(pid: usize) -> WaitSys {
    let mut reaped: Option<Box<Task>> = None;
    let out: WaitSys;
    {
        let _ = unsafe { TrapGuard::save() };
        let mut table = TASK_TABLE.lock();
        // ① 重放入口：Reap/kill 已写 wait_result 并唤醒（等待期间目标退出/被杀）。
        //    读走即清（replace Pending），复位运行态（唤醒时 wake_task 已置 Rerun，
        //    这里补 wait_pid 清理，防御首次进入时陈旧残留）。
        let woke = table.current_mut().and_then(|t| {
            match core::mem::replace(&mut t.wait_result, WaitResult::Pending) {
                WaitResult::Exited(code) => {
                    t.pending = Pending::Rerun;
                    t.wait_pid = None;
                    Some(WaitSys::Code(code as usize))
                }
                WaitResult::Killed => {
                    t.pending = Pending::Rerun;
                    t.wait_pid = None;
                    Some(WaitSys::NoTask)
                }
                WaitResult::Pending => None,
            }
        });
        if let Some(w) = woke {
            out = w;
        } else if let Some(z) = table.take_zombie(pid) {
            // ② 收尸：目标已退出且僵尸保留（父存活未 wait）。exit_code 为 None
            //    说明已被 Reap 交付（等效 ECHILD），防御路径。
            let code = z.exit_code;
            reaped = Some(z);
            out = match code {
                Some(c) => WaitSys::Code(c as usize),
                None => WaitSys::NoTask,
            };
        } else if !table.alive(pid) {
            // ③ 判活：目标不存在（从未存在 / 已被 kill 回收 / 已收尸）→ 不阻塞
            out = WaitSys::NoTask;
        } else if let Some(t) = table.current_mut() {
            // ④ 置 Wait：park 等 Reap/kill 写入 wait_result。resume_sepc=0 保持
            //    sepc=ecall 地址——唤醒后 sret 重放重入本函数（与 read 阻塞同模式）。
            t.pending = Pending::Wait(pid);
            t.wait_pid = Some(pid);
            t.resume_sepc = 0;
            out = WaitSys::Park;
        } else {
            out = WaitSys::NoTask; // 无当前任务（boot/空闲）：不可等待
        }
    }
    if let Some(z) = reaped {
        reclaim(z);
    }
    out
}
