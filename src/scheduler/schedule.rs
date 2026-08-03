// 调度核心 — round-robin 主循环（scheduler 子模块）
//
// scheduler() 是时钟中断的处理入口：回收上次 park 的僵尸，按当前任务
// state 处置（Ready 重排 / Blocked 入睡眠列表 / Zombie 入僵尸列表），唤醒
// 到期 sleeper，选下一就绪任务——队列空时区分两种情况：还有睡眠/僵尸任务
// → 回退等待帧 wfi 空转（工作未完，不能关机）；所有调度列表全空 → 创建
// 空闲任务执行关机。切换地址空间并返回下一个 TrapFrame 给 trap_vector 恢复。

use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::arch::asm;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    context::TrapFrame, hal::csr::sstatus::Sstatus, info, memory, memory::allocator::frame,
};
use crate::{debug, warn};

use super::sleep::{in_dram, now_ticks, wake_task};
use super::task::{
    Pending, Task, TaskKind, TaskState, CURRENT, SLEEP_LIST, TASK_QUEUE, ZOMBIE_LIST,
};

/// 空闲任务入口 — 所有调度列表（就绪/睡眠/僵尸）都空时创建空闲任务，执行关机。
///
/// 全空意味着系统再无任何工作：SBI system_reset 关机。若 SBI 调用失败会陷入
/// wfi 死循环（sbi.rs 兜底），不会错误地继续调度。
fn idle() -> ! {
    warn!("No Task Idle");
    info!("all tasks finished — shutting down");
    crate::sbi::system_reset(crate::sbi::RESET_TYPE_SHUTDOWN, 0)
}

/// 等待帧入口 — 就绪队列空但仍有睡眠/僵尸任务时的 wfi 空转。
///
/// 不算空闲（还有未完成工作，不能关机）：wfi 等下一个时钟中断——届时可能
/// 唤醒到期 sleeper、回收僵尸，再重选。
fn wait() -> ! {
    debug!("no runnable task, waiting for next tick");
    loop {
        unsafe { asm!("wfi") }
    }
}

/// 空闲任务栈 — trap 入口（保存帧 + trap_handler 调用链）与合成帧共用。
/// 恢复时 sp 恒重置为栈顶；合成帧写入栈顶下方的 trap 槽。
static IDLE_STACK: [u8; crate::memory::TASK_STACK_SIZE] = [0; crate::memory::TASK_STACK_SIZE];

/// 写入回退任务合成帧到 IDLE_STACK 的 trap 槽并返回其指针。
///
/// `sepc` 由调用方决定：等待（[`wait`] wfi 空转）或全空关机（[`idle`]）。
/// 内容其余恒定：sp=栈顶、sstatus=SPP|SPIE（sret 回 S-mode 且开中断，时钟
/// 可再触发）；其余寄存器任意（入口函数从零开始执行，不依赖旧值）。
///
/// 每次回退时重新写入——上次内容已被 trap 入口在**同一槽**覆盖（合成帧
/// 与硬件保存帧共用 trap 槽，自洽），故无持久状态、无运行时记忆。
fn idle_frame(sepc: usize) -> NonNull<TrapFrame> {
    let top = IDLE_STACK.as_ptr() as usize + IDLE_STACK.len();
    let slot = (top - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame;
    // SAFETY: slot 落在 IDLE_STACK 内（最后一个 TrapFrame 槽），独占写入。
    unsafe {
        slot.write(TrapFrame {
            ra: 0,
            sp: top,
            gp: 0,
            tp: 0,
            t0: 0,
            t1: 0,
            t2: 0,
            s0: 0,
            s1: 0,
            a0: 0,
            a1: 0,
            a2: 0,
            a3: 0,
            a4: 0,
            a5: 0,
            a6: 0,
            a7: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            s9: 0,
            s10: 0,
            s11: 0,
            t3: 0,
            t4: 0,
            t5: 0,
            t6: 0,
            sepc,
            sstatus: Sstatus::SPIE.bits() | Sstatus::SPP.bits(),
        });
        NonNull::new_unchecked(slot)
    }
}

/// 调度器返回给 trap_vector 的下一个 TrapFrame 地址。
///
/// 用 static 而非栈局部：`switch_space` 切换地址空间后，当前任务栈的 VA
/// （per-task 栈窗口 0xC0000000）会**别名到新任务的栈**——任何栈上局部在
/// switch 之后都读到新任务栈的内存。static 位于 DRAM 恒等映射区，所有
/// 空间一致可见，switch 后读取安全。
static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0);

/// 调度器入口 — 时钟中断处理中调用。
///
/// 状态机主循环：处置当前任务（Idle 不重排；Running 按 [`Pending`] 迁移到
/// 目标队列），唤醒到期 sleeper，选下一就绪任务（空则回退空闲快照），切换
/// 地址空间，返回新任务的 TrapFrame 指针。
pub fn scheduler(frame: *mut TrapFrame) -> usize {
    // 先回收上次 park 的 zombie 栈：此刻执行在**当前**任务的栈上，列表里
    // 只有之前已退出任务的栈，不会释放自己正在用的栈。
    reclaim_zombies();

    let now = now_ticks();
    let mut queue = TASK_QUEUE.lock();
    let mut current = CURRENT.lock();

    // 处置当前任务。state 恒为 Running 或 Idle（boot 首次被抢占前为 None）：
    //   Idle（空闲回退）→ 不重排；None（boot）→ 建空闲快照，此后 CURRENT 恒有值。
    match current.take() {
        Some(task) if task.state == TaskState::Idle => {}
        Some(mut task) => {
            debug_assert!(
                task.state == TaskState::Running,
                "CURRENT task must be Running, got {:?}",
                task.state
            );
            // 用新鲜帧替换 exit/sleep 置过的旧帧。null 帧（terminate_current
            // 栈破坏路径：仅标记 Zombie 并 dispatch）不写入——僵尸任务保留旧帧。
            if let Some(f) = NonNull::new(frame) {
                task.frame = f;
            }
            match task.pending {
                Pending::Rerun => {
                    task.state = TaskState::Ready;
                    queue.push_back(task); // 普通抢占：重排
                }
                Pending::Park if task.wake_tick <= now => {
                    wake_task(&mut task); // 已到期：立即醒（置 Ready）
                    queue.push_back(task);
                }
                Pending::Park => {
                    task.state = TaskState::Blocked;
                    SLEEP_LIST.lock().push_back(task); // park
                }
                Pending::Reap => {
                    task.state = TaskState::Zombie;
                    ZOMBIE_LIST.lock().push_back(task);
                }
            }
        }
        None => {
            // boot 任务（CURRENT=None）首次被抢占：idle 完全无状态（合成帧
            // 每次现场构造），无需记录任何信息，直接由下方选中的 next 填充。
        }
    }

    // 到期 sleeper 移回就绪队列（全遍历，不依赖 SLEEP_LIST 有序）
    wake_sleepers(&mut queue, now);

    // 选下一任务；就绪队列空 → 回退空闲任务（main 的 wfi 空转，保持 Idle）
    let next = match queue.pop_front() {
        Some(mut t) => {
            t.state = TaskState::Running; // 选中即运行
            t.pending = Pending::Rerun; // 正常运行：下次 tick 重排
            t
        }
        None => {
            // 就绪队列空。区分两种情况：
            //   还有睡眠任务待醒 / 僵尸任务待收 → 工作未完，回退等待帧 wfi
            //     空转，等下一个 tick 重选（不能关机）；
            //   所有调度列表全空 → 创建空闲任务，执行关机。
            let sepc = if SLEEP_LIST.lock().is_empty() && ZOMBIE_LIST.lock().is_empty() {
                idle as *const () as usize
            } else {
                wait as *const () as usize
            };
            Task {
                id: 0,
                state: TaskState::Idle,
                pending: Pending::Rerun,
                kind: TaskKind::SMode,
                frame: idle_frame(sepc),
                space: None,
                stack: None,
                stack_size: 0,
                wake_tick: 0,
                resume_sepc: 0,
            }
        }
    };
    // 提取 next 的帧与根页表，再 move 进 CURRENT（Task 非 Copy，move 后不可再读）。
    let next_frame = next.frame.as_ptr() as usize;
    let next_root = match next.space.as_ref() {
        Some(sp) => sp.root_page(),
        None => crate::memory::space::kernel_space()
            .as_ref()
            .map(|ks| ks.root_page())
            .unwrap_or(0),
    };
    current.replace(next);

    // 切换地址空间到新任务。返回值必须在 switch **之前**存入 NEXT_FRAME：
    // switch 后当前任务栈的 VA（per-task 窗口）别名到新任务栈，任何栈上
    // 局部都会读到新任务栈的内存。static 在 DRAM 恒等区，switch 后读取仍正确。
    NEXT_FRAME.store(next_frame, Ordering::Relaxed);
    // SAFETY: 关中断状态，单 hart，新任务的页表应包含代码映射（None → KERNEL_SPACE）
    unsafe {
        memory::switch_space(next_root, 0);
    }
    // switch 后校验硬件状态：satp 的 PPN 已切换到目标空间的根页表。
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
        if let Some(stack) = z.stack {
            // 校验回收 layout 与 spawn 分配时一致（页对齐 + 在 DRAM）——
            // 不符则 deallocate 会把垃圾地址还给分配器，后续分配就崩。
            debug_assert!(
                in_dram(stack.as_ptr() as usize)
                    && (stack.as_ptr() as usize).is_multiple_of(crate::memory::PAGE_SIZE),
                "reclaim: zombie stack {:#p} misaligned or outside DRAM",
                stack.as_ptr(),
            );
            // SAFETY: 僵尸已 park 且不再恢复；`stack` 正是 spawn 时
            // frame 分配器给出的原分配基址，layout 与分配时一致。
            unsafe {
                frame::allocator().deallocate(
                    stack,
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
