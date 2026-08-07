// 阻塞-唤醒状态机（scheduler 子模块）
//
// sleep() 把当前任务置 Blocked 后 wfi 等 tick；调度核心经 wake_task 唤醒
// 到期任务（置 Ready + sepc 重置到 resume_after_sleep）。wait()（见 wait.rs）
// 复用同一机制：事件唤醒时 sepc 重置到 resume_after_wait。时刻读取与
// Duration→ticks 换算收敛至 crate::clock（唯一时间入口）。另含跨任务
// 物理帧访问辅助（frame_phys / in_dram）。

use core::mem::size_of;
use core::ptr::NonNull;
use core::time::Duration;

use crate::{context::TrapFrame, debug, lock::TrapGuard};

use super::task::{Pending, TASK_TABLE, Task, TaskState};

/// 任务 TrapFrame 的物理地址（`NonNull`：恒非空——堆栈推导或 idle 帧）。
///
/// 唤醒等"在其它任务空间激活时"的场合，任务自己的帧 VA 在当前空间
/// 不可见（会被静默映射到当前任务的栈顶帧），必须经物理地址访问——所有
/// 物理 DRAM 恒为各空间 identity 映射（`from_kernel` 全量复制 L2[2]），
/// 物理写在任何活动空间下都正确。空闲任务（space=None）栈在 boot 栈上
/// （identity，VA==PA），直接用它存的 frame 指针。
fn frame_phys(t: &Task) -> NonNull<TrapFrame> {
    if t.space.is_some() {
        // 有空间的任务必有堆栈（spawn 分配）；帧在栈顶物理地址
        let base = t
            .stack
            .expect("task with space must have heap stack")
            .as_ptr() as usize;
        NonNull::new((base + t.stack_size - size_of::<TrapFrame>()) as *mut TrapFrame)
            .expect("frame at stack top is never null")
    } else {
        t.frame
    }
}

/// 地址是否落在 DRAM 恒等映射区（跨任务物理访问的前提）。
///
/// 只在 `debug_assert!` 里引用；release 下断言折叠为 `if false` 分支，但
/// 该分支仍要解析本名称，故定义恒保留（无调用点时压掉 dead_code 警告）。
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) fn in_dram(addr: usize) -> bool {
    let cfg = crate::platform::get();
    (cfg.dram_base..cfg.dram_base + cfg.dram_size).contains(&addr)
}

/// 唤醒一个 Blocked 任务：置 Ready 并把 sepc 重置到恢复点。
///
/// **不能对保存的 sepc 做 `+=4`**：`wfi` 是 hint，若中断在 `csrs SIE` 时已挂起，
/// 它直接完成（不阻塞），trap 落在 wfi 的下一条指令——保存的 sepc 可能是
/// wfi 或其后的任意指令。无条件重置到 [`resume_after_sleep`]（仅含 `ret`）
/// 对"阻塞"与"立即返回"两种情形都正确，且与指令宽度（压缩指令）无关。
///
/// `resume_sepc == 0` 时跳过 sepc 重置（UMode 输入等待的重放模式：保持帧
/// sepc 原值——ecall 指令地址，sret 重放分发，见 [`input_wait`]）；sleep/
/// wait 的 resume_sepc 恒非 0，不受影响。
pub(crate) fn wake_task(t: &mut Task) {
    t.state = TaskState::Ready;
    t.pending = Pending::Rerun;
    let fp = frame_phys(t);
    // 校验帧物理地址在 DRAM——跨任务物理写（frame_phys）的前提，把"写错
    // 内存"提前到 wake 当场而非事后崩在调度目标上。
    debug_assert!(
        in_dram(fp.as_ptr() as usize),
        "wake: frame {:#x} outside DRAM — cross-task physical write would hit garbage",
        fp.as_ptr() as usize,
    );
    if t.resume_sepc != 0 {
        // SAFETY: 任务已 park 且空间未激活，其帧 VA 在当前空间不可见；经
        // 物理地址写（frame_phys，identity 映射下任意活动空间可见）。
        unsafe {
            (*fp.as_ptr()).sepc = t.resume_sepc;
        }
    }
    debug!("wake task {} (sepc {:#x})", t.id, t.resume_sepc);
}

/// 仅含一条 `wfi` 的辅助函数：wfi 是 hint，可能阻塞至中断，也可能因
/// 中断已挂起而直接返回。sleep() 与 wait() 共用。
#[inline(never)]
pub(crate) fn sleep_wfi() {
    // SAFETY: wfi 不触碰内存/栈。
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

/// 唤醒恢复点：仅含一条 `ret`。
///
/// wake_task 把 Blocked 任务的 sepc 重置到此处；`ret` 借助保存的 `ra` 跳回
/// sleep() 调用方。若 trap 落在 wfi（`ra` = sleep 内调用点之后），经 sleep
/// 的 epilogue 正常返回；若落在 `csrs SIE` 与 wfi 之间的窗口（`ra` 尚未被
/// 内部调用修改），则直接跳回调用方——两条路径都正确继续。
#[inline(never)]
fn resume_after_sleep() {}

/// 事件唤醒恢复点：仅含一条 `ret`（对应 [`resume_after_sleep`] 的时间版）。
///
/// wait(pid) 阻塞期间，子退出（Reap 处置）或子被 kill（kill 路径）时，
/// wake_task 把等待者的 sepc 重置到此处；`ret` 借助保存的 `ra` 跳回 wait()
/// 调用方，恢复段读取 [`crate::schedule::task::WaitResult`] 后返回。
#[inline(never)]
pub(crate) fn resume_after_wait() {}

/// 输入唤醒恢复点：仅含一条 `ret`（对应 [`resume_after_wait`] 的输入版）。
///
/// SMode 内核任务阻塞读（[`input_wait`]）被唤醒时，wake_task 把 sepc 重置
/// 到此处；`ret` 借助保存的 `ra` 跳回 input_wait() 调用方，恢复段读取输入
/// 缓冲后返回。UMode 任务不走本恢复点（resume_sepc = 0 → 重放 ecall）。
#[inline(never)]
pub(crate) fn resume_after_read() {}

/// 置当前任务为输入等待（pending = WaitRead，事件唤醒维度永不过期）。
///
/// 恢复模式按任务类型区分：
///   - SMode（内核任务，普通运行上下文）→ `resume_sepc = resume_after_read`：
///     唤醒时 wake_task 重置 sepc，sret 到恢复点 ret 回调用方（同 wait 机制）；
///   - UMode（U 任务 ecall 分发，**trap 上下文 SIE=0**）→ `resume_sepc = 0`：
///     唤醒不动 sepc——任务被 tick 切走再唤醒后 sret 到 ecall 指令**重放**
///     分发（trap 上下文不能 wfi，见 [`crate::runtime::envcall`]），dispatch
///     重入时缓冲已非空直接读返回（幂等，无数据丢失/重复）。
///
/// 只置状态不等待：SMode 由 [`input_wait`] 在置位后 wfi；UMode 由 envcall
/// 返回后经 trap_handler 重放 ecall（用户态忙转直到 tick 抢占 park）。
pub(crate) fn mark_input_wait() {
    let resume = if crate::schedule::current_is_umode() {
        0
    } else {
        resume_after_read as *const () as usize
    };
    // 临界区：置 WaitRead + 唤醒点必须原子（关中断）完成——保证第一个
    // 可能落地的 tick 看到的已是 WaitRead；否则任务带着 Rerun 被抢占，
    // 输入等待静默失效。TrapGuard 按进入前状态恢复 SIE（配对由 RAII 保证）。
    {
        let _ = unsafe { TrapGuard::save() };
        let mut table = TASK_TABLE.lock();
        if let Some(t) = table.current_mut() {
            t.pending = Pending::WaitRead;
            t.wake_tick = u64::MAX; // 事件唤醒，时间维度永不过期
            t.resume_sepc = resume;
        }
    }
}

/// 复位当前任务的输入等待标记（pending 为 WaitRead 时 → Rerun）。
///
/// 幂等：非 WaitRead 状态不动作。SMode 的 [`input_wait`] 恢复段与 UMode
/// 的 envcall 读到数据后（重放循环结束）各调一次。
pub(crate) fn clear_input_wait() {
    let _ = unsafe { TrapGuard::save() };
    let mut table = TASK_TABLE.lock();
    if let Some(t) = table.current_mut()
        && t.pending == Pending::WaitRead
    {
        t.pending = Pending::Rerun;
    }
}

/// 当前任务是否处于输入等待（pending == WaitRead）。
///
/// trap_handler 的 envcall 分支据此决定是否跳过 ecall（重放）：WaitRead →
/// 不加 sepc，sret 重放 ecall（缓冲空，继续等）；否则 +4 正常返回。
pub(crate) fn is_input_waiting() -> bool {
    TASK_TABLE
        .lock()
        .current()
        .is_some_and(|t| t.pending == Pending::WaitRead)
}

/// 阻塞当前任务直到输入缓冲非空（console 输入等待，对应 Linux tty_read 的睡眠）。
///
/// 由 [`mark_input_wait`] 置位后 wfi 等中断，唤醒（wake_task 重置 sepc 到
/// resume 点 ret 回调用方，或 UMode 重放）后经恢复段复位标记返回。
/// **仅限普通运行上下文（SIE=1）调用**——trap 上下文（SIE=0）wfi 永不醒，
/// UMode 任务走 envcall 的 mark + 重放路径，不经本函数。
///
/// 约束：不持有任何锁时调用；不得由 boot/空闲任务调用（CURRENT 为空时
/// 置位静默无效，任务将带着 Rerun 继续运行）。
pub(crate) fn input_wait() {
    mark_input_wait();
    sleep_wfi();
    // 恢复段：wfi 是 hint——中断已挂起时直接返回（任务从未被 park，pending
    // 仍是 WaitRead），若带着 WaitRead 继续运行，下次抢占会被误 park 进睡眠
    // 列表。统一复位 Rerun 对两条路径都正确（唤醒时 wake_task 已复位，此
    // 判断不触发）。关中断缩小"复位前被抢占"的窗口。
    clear_input_wait();
}

/// 唤醒所有阻塞在输入等待的任务（输入字符到达时，中断上下文调用）。
///
/// 遍历睡眠队列移出 `pending == WaitRead` 的任务 → wake_task（置 Ready +
/// 按 resume_sepc 重置 sepc；`resume_sepc == 0` 的 UMode 任务保持 sepc 不变
/// → sret 到 ecall 重放分发）。
///
/// 当前任务（CURRENT）不在睡眠队列——wfi 自旋中的等待者由恢复段自行复位，
/// 不经本函数处理（任务仍在运行，无需"唤醒"）。
///
/// 中断上下文安全：SpinLock 关中断、单 hart 无抢占；TASK_TABLE 在
/// handle_interrupt 期间未被持有。
pub(crate) fn wake_input_waiters() {
    let mut table = TASK_TABLE.lock();
    for mut t in table.take_input_waiters() {
        wake_task(&mut t);
        table.push_ready(t);
    }
}

/// 阻塞当前任务一段时长（[`Duration`]，按 timebase 频率换算为 mtime 刻度）。
///
/// 换算公式：`ticks = secs × freq + subsec_nanos × freq / 1e9`（saturating，
/// 超大时长不 panic，只睡到 u64 能表达的极限）。例如 `sleep(Duration::from_secs(1))`
/// 在 10 MHz timebase 上阻塞 10_000_000 个 mtime 刻度，即 1 秒。
///
/// 实现：关中断原子地置 `pending = Park` + 唤醒点，开中断后 `wfi` 等 tick；
/// 到期时 wake_task 把 sepc 重置到 [`resume_after_sleep`]，本函数正常返回。
/// `wfi` 是 hint（中断已挂起时可能直接返回），恢复段统一复位 pending，
/// 两条路径都正确。
///
/// 约束：不持有任何锁时调用（SIE=0 时 wfi 永不醒）；不得由 boot/空闲任务
/// 调用（CURRENT 为空时置位静默无效，任务将带着 Rerun 继续运行）。
pub fn sleep(d: Duration) {
    // Duration → mtime 刻度换算收敛至 clock（唯一换算处）
    let deadline = crate::clock::now().saturating_add(crate::clock::duration_to_ticks(d));

    // 临界区：置 Park + 唤醒点必须原子（关中断）完成——保证第一个可能落地
    // 的 tick 看到的已是 Park；否则任务带着 Rerun 被抢占，sleep 静默失效。
    // TrapGuard 按进入前状态恢复 SIE：进入前已关中断时保持关闭（drop 不
    // 误开），配对由 RAII 保证，不会像手写 clear/set 那样在嵌套场景误开。
    {
        let _ = unsafe { TrapGuard::save() };
        let mut table = TASK_TABLE.lock();
        if let Some(t) = table.current_mut() {
            t.pending = Pending::Park;
            t.wake_tick = deadline;
            t.resume_sepc = resume_after_sleep as *const () as usize;
        }
    }
    sleep_wfi();

    // 恢复段：wfi 是 hint——中断已挂起时直接返回（任务从未被 park，pending
    // 仍是 Park），若带着 Park 继续运行，下次抢占会被误 park 进睡眠列表。
    // 统一复位 Rerun 对两条路径都正确（到期唤醒时 wake_task 已复位，此
    // 判断不触发）。关中断缩小"复位前被抢占"的窗口。
    {
        let _ = unsafe { TrapGuard::save() };
        let mut table = TASK_TABLE.lock();
        if let Some(t) = table.current_mut()
            && t.pending == Pending::Park
        {
            t.pending = Pending::Rerun;
        }
    }
}
