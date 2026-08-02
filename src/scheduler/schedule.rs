// 调度核心 — round-robin 主循环（scheduler 子模块）
//
// scheduler() 是时钟中断的处理入口：回收上次 park 的僵尸，按当前任务
// state 处置（Ready 重排 / Blocked 入睡眠列表 / Zombie 入僵尸列表），唤醒
// 到期 sleeper，选下一就绪任务（空则回退空闲任务快照），切换地址空间并
// 返回下一个 TrapFrame 给 trap_vector 恢复。

use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ptr::{null_mut, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{context::TrapFrame, info, memory, memory::allocator::frame};

use super::sleep::{in_dram, now_ticks, wake_task};
use super::task::{CURRENT, IDLE_TASK, SLEEP_LIST, TASK_QUEUE, ZOMBIE_LIST, Task, TaskKind, TaskState};

/// 调度器返回给 trap_vector 的下一个 TrapFrame 地址。
///
/// 用 static 而非栈局部：`switch_space` 切换地址空间后，当前任务栈的 VA
/// （per-task 栈窗口 0xC0000000）会**别名到新任务的栈**——任何栈上局部在
/// switch 之后都读到新任务栈的内存。static 位于 DRAM 恒等映射区，所有
/// 空间一致可见，switch 后读取安全。
static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0);

/// 从空闲任务快照重建一份副本。
///
/// 不 clone Box：idle 的 space 恒为 None（字段全 Copy），重建避免 `Task: Clone`
/// 的共享页表陷阱（克隆会共享页表根 → double-free）。
fn idle_snapshot() -> Option<Task> {
    let idle = IDLE_TASK.lock();
    idle.as_ref().map(|i| {
        debug_assert!(i.space.is_none(), "idle snapshot must not own a space");
        Task {
            id: i.id,
            state: i.state,
            kind: i.kind,
            frame: i.frame,
            space: None,
            stack: i.stack,
            stack_size: i.stack_size,
            wake_tick: i.wake_tick,
            resume_sepc: i.resume_sepc,
        }
    })
}

/// 调度器入口 — 时钟中断处理中调用。
///
/// 按当前任务状态处置：Ready 重排、Blocked 到期即醒/未到期入睡眠列表、
/// Zombie 入僵尸列表；然后选下一就绪任务（空则回退空闲任务），切换地址空间，
/// 返回新任务的 TrapFrame 指针。
pub fn scheduler(frame: *mut TrapFrame) -> usize {
    // ① 先回收上次 park 的 zombie 栈：此刻执行在**当前**任务的栈上，
    //    列表里只有之前已退出任务的栈，不会释放自己正在用的栈。
    reclaim_zombies();

    let now = now_ticks();
    let mut q = TASK_QUEUE.lock();
    let mut cur = CURRENT.lock();
    let is_idle = IDLE_TASK.lock().as_ref().is_some_and(|i| i.frame == frame);

    if is_idle {
        // 空闲任务被抢占：不重排，直接选下一就绪任务
    } else if let Some(mut task) = cur.take() {
        task.frame = frame; // 用新鲜帧替换 exit/sleep 置过的旧帧
        match task.state {
            TaskState::Zombie => ZOMBIE_LIST.lock().push_back(task),
            TaskState::Blocked => {
                if task.wake_tick <= now {
                    wake_task(&mut task);
                    q.push_back(task); // 已到期：立即醒
                } else {
                    SLEEP_LIST.lock().push_back(task); // park
                }
            }
            TaskState::Ready => q.push_back(task), // 普通抢占：重排
        }
    } else {
        // boot 任务（CURRENT=None）首次被抢占 → 建空闲任务快照，不入队
        let idle = Task {
            id: 0,
            state: TaskState::Ready,
            kind: TaskKind::SMode,
            frame,
            space: None,
            stack: null_mut(),
            stack_size: 0,
            wake_tick: 0,
            resume_sepc: 0,
        };
        *IDLE_TASK.lock() = Some(idle);
        *cur = idle_snapshot();
    }

    // ② 到期 sleeper 移回就绪队列（全遍历，不依赖 SLEEP_LIST 有序）
    wake_sleepers(&mut q, now);

    // ③ 选下一任务；就绪队列空 → 回退空闲任务（idle_snapshot 重建副本）
    let mut next = q.pop_front().or_else(idle_snapshot).unwrap_or_else(|| {
        // 防御：boot 首次被抢占即建空闲快照，此后 IDLE_TASK 恒为 Some。
        panic!("scheduler: no runnable task");
    });
    next.state = TaskState::Ready; // CURRENT 存在即 running
    // 提取 next 的帧与根页表，再 move 进 CUR（Task 非 Copy，move 后不可再读）。
    let next_frame = next.frame as usize;
    let next_root = match next.space.as_ref() {
        Some(sp) => sp.root_page(),
        None => crate::memory::space::kernel_space()
            .as_ref()
            .map(|ks| ks.root_page())
            .unwrap_or(0),
    };
    *cur = Some(next);

    // ④ 切换地址空间到新任务。返回值必须在 switch **之前**存入 NEXT_FRAME：
    //    switch 后当前任务栈的 VA（per-task 窗口）别名到新任务栈，任何栈上
    //    局部都会读到新任务栈的内存。static 在 DRAM 恒等区，switch 后读取仍正确。
    NEXT_FRAME.store(next_frame, Ordering::Relaxed);
    // SAFETY: 关中断状态，单 hart，新任务的页表应包含代码映射（None → KERNEL_SPACE）
    unsafe {
        memory::switch_space(next_root, 0);
    }
    // ⑤ switch 后校验硬件状态：satp 的 PPN 已切换到目标空间的根页表。
    // 把"切了没切对"从下一次地址访问（可能已破坏内存）提前到 switch 当场。
    debug_assert_eq!(
        unsafe { crate::hal::csr::satp::read() } & 0x000F_FFFF_FFFF,
        next_root,
        "scheduler: satp PPN mismatch after switch_space",
    );
    NEXT_FRAME.load(Ordering::Relaxed)
}

/// 把到期任务从睡眠队列移回就绪队列。
fn wake_sleepers(q: &mut VecDeque<Task>, now: u64) {
    let due: Vec<Task> = {
        let mut sl = SLEEP_LIST.lock();
        let mut due = Vec::new();
        let mut pending = Vec::new();
        for t in sl.drain(..) {
            if t.wake_tick <= now {
                due.push(t);
            } else {
                pending.push(t);
            }
        }
        for t in pending {
            sl.push_back(t);
        }
        due
    };
    for mut t in due {
        wake_task(&mut t);
        q.push_back(t);
    }
}

/// 回收僵尸任务栈。
///
/// 在 scheduler() 开头调用：此刻执行在**当前**任务的栈上，被回收的栈都是
/// 之前 park 的僵尸（本次 park 的当前僵尸在这一步之后才入列表），不会自释放。
fn reclaim_zombies() {
    let zombies = {
        let mut zl = ZOMBIE_LIST.lock();
        core::mem::take(&mut *zl)
    };
    for z in zombies {
        if z.stack_size > 0 {
            // 校验回收 layout 与 spawn 分配时一致（页对齐 + 在 DRAM）——
            // 不符则 deallocate 会把垃圾地址还给分配器，后续分配就崩。
            debug_assert!(
                in_dram(z.stack as usize)
                    && (z.stack as usize).is_multiple_of(crate::memory::PAGE_SIZE),
                "reclaim: zombie stack {:#p} misaligned or outside DRAM",
                z.stack,
            );
            // SAFETY: 僵尸已 park 且不再恢复；`z.stack` 正是 spawn 时
            // frame 分配器给出的原分配基址，layout 与分配时一致。
            unsafe {
                frame::allocator().deallocate(
                    NonNull::new(z.stack).unwrap(),
                    Layout::from_size_align(z.stack_size, crate::memory::PAGE_SIZE).unwrap(),
                );
            }
            info!("reclaimed stack of task {}", z.id);
        }
        // 释放任务独占的地址空间（Box 所有权 → drop 回收私有页表树 + regions）。
        // 此刻 Zombie 已切走、地址空间非活动，私有页表帧归还 page 分配器；
        // 共享的内核页表（DRAM identity/MMIO/高半区）由 clean 的 skip 保护。
        if let Some(sp) = z.space {
            drop(sp);
            info!("reclaimed address space of task {}", z.id);
        }
    }
}
