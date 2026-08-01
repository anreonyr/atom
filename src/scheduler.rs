// 任务调度器 — trap 驱动的 round-robin 抢占 + 就绪/阻塞/僵尸状态机
//
// 时钟中断 → trap_vector 把上下文保存到任务栈顶的 TrapFrame → scheduler(frame)：
// 按当前任务的 state 决定其去向（Ready 重排 / Blocked 入睡眠列表 / Zombie 入僵尸列表），
// 弹出下一就绪任务，切换地址空间，返回其 TrapFrame 供 trap_vector 恢复。
//
// 每个任务运行在独立的地址空间（spawn 自动建 `from_kernel` 克隆），栈映射到
// 固定虚拟窗口 [`TASK_STACK_BASE`, +STACK_SIZE)，窗口下方一页留空作守护页——
// 栈溢出直接触发缺页（user → terminate / kernel → panic）。
// `current_space()` 从 CURRENT 推导，缺页处理器据此路由。

use alloc::boxed::Box;
use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::mem::size_of;
use core::ptr::{null_mut, NonNull};
use core::time::Duration;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    debug, info,
    lock::SpinLock,
    memory,
    memory::{
        addr::{PhysAddr, VirtAddr},
        allocator::{frame, page},
        entry::PteFlags,
        space::AddressSpace,
    },
    context::TrapFrame,
};

/// 任务在下一个调度点的去向。
///
/// Running 由"任务在 `CURRENT` 里"隐含，不单独编码；就绪/阻塞/僵尸队列的
/// 列表归属已决定其状态，state 字段只表达 CURRENT 任务在 tick 时的处置。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskState {
    /// 重排就绪队列
    Ready,
    /// 阻塞至 `wake_tick`（sleep）
    Blocked,
    /// 已退出，待回收栈
    Zombie,
}

/// 任务属性：决定同步异常（缺页/非法指令等）的处置方向。
///
/// U-mode 尚未落地，所有任务当前都运行在 S-mode（spawn 的初始帧置
/// SPP=Supervisor）；枚举表达的是任务的**目标模式 / 异常处置策略**，
/// 而非运行时特权级。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TaskKind {
    /// SMode 任务（内核）：同步异常 → panic（内核 bug，崩溃可诊断）
    SMode,
    /// UMode 任务（用户）：同步异常 → terminate_current（等效 SIGSEGV，系统继续）
    UMode,
}

/// 调度队列条目。
///
/// 不实现 `Clone`：`space` 持有独占所有权（Box），克隆会共享页表根导致
/// double-free。队列操作全部 move；空闲快照副本在 [`scheduler`] 内重建。
pub(crate) struct Task {
    pub id: usize,
    pub state: TaskState,
    pub kind: TaskKind,
    pub frame: *mut TrapFrame,
    /// 所属地址空间；None = 内核空间（KERNEL_SPACE，仅空闲/boot 任务）。
    ///
    /// 任务**独占**空间所有权（spawn 创建 / spawn_with 传入，Box 类型系统
    /// 强制一空间一任务）——Zombie 回收时 drop，触发页表树归还 page 分配器。
    pub space: Option<Box<AddressSpace>>,
    /// 栈物理基址，zombie 回收与 [`frame_phys`] 用（boot 任务不在堆上，为 null）
    pub stack: *mut u8,
    pub stack_size: usize,
    /// Blocked 时的唤醒时间（mtime 刻度）
    pub wake_tick: u64,
    /// 唤醒后的恢复点（sepc 重置目标）— 见 [`sleep`]
    pub resume_sepc: usize,
}

/// 就绪队列（Round-robin）。
static TASK_QUEUE: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 睡眠队列 — Blocked 任务，到期后由 `wake_sleepers` 移回就绪队列。
static SLEEP_LIST: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 僵尸队列 — 已退出任务，下一个 scheduler() 开头回收其栈。
static ZOMBIE_LIST: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 当前运行任务。存在即 running；缺页处理器经 `current_space()` 由它推导活动地址空间。
///
/// TODO: 多 hart 时应改为 per-hart 数组 [SpinLock<Option<Task>>; MAX_HARTS]，
///       按 hart_id 索引，避免不同 hart 的 CURRENT 互相覆盖。
static CURRENT: SpinLock<Option<Task>> = SpinLock::new(None);

/// 空闲任务 — boot 任务（main）首次被抢占时建立的快照。
///
/// 空闲任务永不入就绪队列；就绪队列为空时回退到它（main 的 wfi 空转）。
static IDLE_TASK: SpinLock<Option<Task>> = SpinLock::new(None);

/// 任务 id 分配器。
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// 每个任务栈的大小（字节）
pub(crate) const STACK_SIZE: usize = 16384;

/// 任务栈虚拟窗口基址（Sv39 低半区，L2 索引 3）。
///
/// 内核仅映射 L2 0/1/2（MMIO / PCIe / DRAM）+ 高半区；L2[3] 未映射 →
/// `from_kernel` 浅克隆后各任务克隆里该条目无效 → 每个任务映射栈时各自
/// 分配私有 L1/L0，同一 VA 互不覆盖。守护页 = [BASE-4K, BASE) 保持未映射
/// （该页落在共享 L2[2] 子树的 L1 索引 504，靠"内核永不映射之"保证缺页）。
///
/// 不变量：内核不得在内核空间映射 L2[3]（0xC000_0000..0x4000_0000）；
/// DRAM 必须 < 1 GiB（否则与 DRAM 重叠；QEMU virt 默认 128 MiB）。
pub(crate) const TASK_STACK_BASE: usize = 0xC000_0000;

/// 当前时刻（mtime 刻度，来自 CLINT/`time` CSR）。
fn now_ticks() -> u64 {
    crate::hal::interrupt::get_internal()
        .map(|ii| ii.read())
        .unwrap_or(0)
}

/// 调度器返回给 trap_vector 的下一个 TrapFrame 地址。
///
/// 用 static 而非栈局部：`switch_space` 切换地址空间后，当前任务栈的 VA
/// （per-task 栈窗口 0xC0000000）会**别名到新任务的栈**——任何栈上局部在
/// switch 之后都读到新任务栈的内存。static 位于 DRAM 恒等映射区，所有
/// 空间一致可见，switch 后读取安全。
static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0);

/// 查询当前活动地址空间：从 [`CURRENT`] 推导，None = 内核空间。
///
/// 缺页处理器用它在内核空间与用户空间之间路由。调度器在 dispatch 时切换
/// 地址空间并更新 CURRENT，这里始终反映"正在运行的任务"的空间。
/// 查询当前活动地址空间：从 [`CURRENT`] 推导，None = 内核空间。
///
/// 返回裸指针：借用无法脱离锁的临时生命周期。`CURRENT` 持有空间所有权
/// （Box），堆数据地址稳定、锁释放后仍有效，缺页处理器在 trap 上下文使用。
pub fn current_space() -> Option<*const AddressSpace> {
    CURRENT
        .lock()
        .as_ref()
        .and_then(|c| c.space.as_deref().map(|s| s as *const AddressSpace))
}

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

    // ③ 选下一任务；就绪队列空 → 回退空闲任务
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

/// 任务 TrapFrame 的物理地址。
///
/// 唤醒/回收等"在其它任务空间激活时"的场合，任务自己的帧 VA 在当前空间
/// 不可见（会被静默映射到当前任务的栈顶帧），必须经物理地址访问——所有
/// 物理 DRAM 恒为各空间 identity 映射（`from_kernel` 全量复制 L2[2]），
/// 物理写在任何活动空间下都正确。空闲任务（space=None）栈在 boot 栈上
/// （identity，VA==PA），直接用它存的 frame 指针。
fn frame_phys(t: &Task) -> *mut TrapFrame {
    if t.space.is_some() {
        (t.stack as usize + t.stack_size - size_of::<TrapFrame>()) as *mut TrapFrame
    } else {
        t.frame
    }
}

/// 地址是否落在 DRAM 恒等映射区（跨任务物理访问的前提）。
///
/// 仅在调试构建编译：只在 `debug_assert!` 里引用，release 下连同调用一起裁剪。
#[cfg(debug_assertions)]
fn in_dram(addr: usize) -> bool {
    let cfg = crate::platform::get();
    (cfg.dram_base..cfg.dram_base + cfg.dram_size).contains(&addr)
}

/// 唤醒一个 Blocked 任务：置 Ready 并把 sepc 重置到恢复点。
///
/// **不能对保存的 sepc 做 `+=4`**：`wfi` 是 hint，若中断在 `csrs SIE` 时已挂起，
/// 它直接完成（不阻塞），trap 落在 wfi 的下一条指令——保存的 sepc 可能是
/// wfi 或其后的任意指令。无条件重置到 [`resume_after_sleep`]（仅含 `ret`）
/// 对"阻塞"与"立即返回"两种情形都正确，且与指令宽度（压缩指令）无关。
fn wake_task(t: &mut Task) {
    t.state = TaskState::Ready;
    // 校验帧物理地址在 DRAM——跨任务物理写（frame_phys）的前提，把"写错
    // 内存"提前到 wake 当场而非事后崩在调度目标上。
    debug_assert!(
        in_dram(frame_phys(t) as usize),
        "wake: frame {:#x} outside DRAM — cross-task physical write would hit garbage",
        frame_phys(t) as usize,
    );
    // SAFETY: 任务已 park 且空间未激活，其帧 VA 在当前空间不可见；经
    // 物理地址写（frame_phys，identity 映射下任意活动空间可见）。
    unsafe {
        (*frame_phys(t)).sepc = t.resume_sepc;
    }
    debug!("wake task {} (sepc {:#x})", t.id, t.resume_sepc);
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

/// 任务态退出（公共 kill API 的唯一入口）：标 Zombie 后 wfi 等 tick 来 park。
///
/// sepc 无关（僵尸永不恢复），无需原子序列。约束：不持有任何锁时调用
/// （SIE=0 时 wfi 永不醒）；不得由 boot/空闲任务调用。
pub fn exit(code: i32) -> ! {
    info!("task {} exit(code={})", current_id(), code);
    {
        let mut cur = CURRENT.lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Zombie;
        }
    }
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// trap 态终止当前任务（trap.rs 未处理用户缺页调用）。
///
/// 与 [`exit`] 是同一个"杀当前任务"语义的 trap 侧入口：trap 态持有刚保存的
/// TrapFrame，可立即 dispatch 下一任务并返回其帧。属跨模块内部辅助。
pub(crate) fn terminate_current(frame: *mut TrapFrame) -> usize {
    {
        let mut cur = CURRENT.lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Zombie;
        }
    }
    scheduler(frame)
}

/// 当前任务 id（无任务时返回 `usize::MAX`）。
fn current_id() -> usize {
    CURRENT.lock().as_ref().map(|c| c.id).unwrap_or(usize::MAX)
}

/// 仅含一条 `wfi` 的辅助函数：wfi 是 hint，可能阻塞至中断，也可能因
/// 中断已挂起而直接返回。
#[inline(never)]
fn sleep_wfi() {
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

/// 阻塞当前任务一段时长（[`Duration`]，按 timebase 频率换算为 mtime 刻度）。
///
/// 换算公式：`ticks = secs × freq + subsec_nanos × freq / 1e9`（saturating，
/// 超大时长不 panic，只睡到 u64 能表达的极限）。例如 `sleep(Duration::from_secs(1))`
/// 在 10 MHz timebase 上阻塞 10_000_000 个 mtime 刻度，即 1 秒。
///
/// 关全局中断置 Blocked 与唤醒时刻，再开中断进入 `wfi`。无论被抢占点落在
/// 何处（wfi 阻塞、wfi 因挂起中断立即返回、或 `csrs`/`wfi` 之间），唤醒时
/// wake_task 都把 sepc 重置到 [`resume_after_sleep`]，本函数正常返回。
///
/// 约束：不持有任何锁时调用（SIE=0 时 wfi 永不醒）。
pub fn sleep(d: Duration) {
    let freq = crate::platform::get().timebase_frequency;
    // Duration → mtime 刻度：整秒部分 × 频率，亚秒部分按比例换算。
    let deadline = now_ticks()
        .saturating_add(d.as_secs().saturating_mul(freq))
        .saturating_add(
            (d.subsec_nanos() as u64)
                .saturating_mul(freq)
                .saturating_div(1_000_000_000),
        );

    // SAFETY: S-mode 下允许开关全局中断。
    unsafe {
        crate::hal::csr::sstatus::clear(crate::hal::csr::sstatus::Sstatus::SIE);
    }
    {
        // TrapGuard 看到 SIE 已关，drop 后不恢复，整个区域中断关闭；
        // 保证第一个可能落地中断看到的已是 Blocked 状态。
        let mut cur = CURRENT.lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Blocked;
            t.wake_tick = deadline;
            t.resume_sepc = resume_after_sleep as *const () as usize;
        }
    }
    // 开中断后进入 wfi。clear/set 两条 asm 均为编译器屏障，
    // 状态置位不会被移出临界区。
    // SAFETY: 只写 sstatus.SIE 位。
    unsafe {
        crate::hal::csr::sstatus::set(crate::hal::csr::sstatus::Sstatus::SIE);
    }
    sleep_wfi();
    // 唤醒后经 resume_after_sleep 的 ret 跳回此处，返回调用方。
}

/// 创建一个新的 SMode（内核）任务：自动创建 per-task 地址空间，带守护页。
///
/// `entry: fn()` 只接受**内核代码段内的函数指针**——fn 指针类型由编译期
/// 保证必然指向内核镜像。加载 ELF 用户程序需要按虚拟地址入口的新 API
/// （`entry: usize` + UMode 空间），落地前此限制保持不变。
pub fn spawn(entry: fn()) {
    spawn_impl(entry, TaskKind::SMode, None);
}

/// 创建一个新任务（UMode 语义，指定地址空间，所有权移交给任务）。
///
/// **任务独占该空间**：Box 所有权进 Task，Zombie 回收时释放（页表树归还）。
/// 同 VA 重复映射会返回 `AlreadyMapped`，每个任务应使用独立的空间。
///
/// `entry: fn()` 同 [`spawn`]：仅接受内核代码段函数指针；ELF 入口（虚拟
/// 地址）需另设 API，当前 UMode 任务以函数指针方式仿真用户语义。
pub fn spawn_with(entry: fn(), space: Box<AddressSpace>) {
    spawn_impl(entry, TaskKind::UMode, Some(space));
}

/// 创建任务：从 frame 分配器申请栈帧，映射到固定虚拟窗口 [`TASK_STACK_BASE`]，
/// 在栈顶构造初始 [`TrapFrame`]，然后将其推入调度队列。
/// 下一次定时器中断发生时，调度器会选中它。
///
/// `kind` 决定同步异常处置：SMode → panic，UMode → terminate_current。
/// `space` 为 None 时自动创建 per-task 地址空间（`from_kernel` 克隆）；
/// 为 Some 时沿用调用方空间（`spawn_with`）。
///
/// `entry` 是一个永不返回的函数指针（不应包含 `ret` 路径），
/// 或应在其生命周期末尾调用 [`exit`]。
///
/// 入口类型暂为 `fn()`（内核代码段指针）：ELF 用户程序加载后需按
/// `usize` 虚拟入口另设 API，见 [`spawn`]/[`spawn_with`] 的说明。
///
/// 新任务的 TrapFrame 配置为 sret 后进入 S-mode 且中断使能。
fn spawn_impl(entry: fn(), kind: TaskKind, space: Option<Box<AddressSpace>>) {
    // ① 从 frame 分配器申请 16 KiB 物理栈帧（order-2，页对齐）
    let stack = frame::allocator()
        .allocate(Layout::from_size_align(STACK_SIZE, crate::memory::PAGE_SIZE).unwrap())
        .expect("spawn: stack allocation failed");
    let stack_pa = stack.as_ptr() as *mut u8 as usize; // 物理基址（瘦化胖指针）

    // ② 确定任务空间：kernel 任务新建私有克隆；user 任务沿用调用方空间（所有权随任务）
    let space: Box<AddressSpace> = match space {
        Some(s) => s,
        None => Box::new(
            AddressSpace::from_kernel(page::allocator()).expect("spawn: clone kernel space failed"),
        ),
    };

    // ③ 把栈映射到固定 VA 窗口；守护页 [BASE-4K, BASE) 不映射（纯虚拟留空）。
    //    不带 G：switch_space 的 sfence.vma 以通用寄存器传 asid（rs2≠x0），
    //    只刷 ASID 0 的非全局条目——带 G 的栈条目跨任务切换会残留
    //    （同 VA 命中上一任务的物理帧）。也无 X（栈不可执行）、无 U。
    space
        .map(
            VirtAddr::from_raw(TASK_STACK_BASE),
            PhysAddr::from_raw(stack_pa),
            STACK_SIZE,
            PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D,
            page::allocator(),
        )
        .expect("spawn: map task stack failed");

    // ③后 映射校验（debug 构建）。把"映射是否生效"从首次调度提前到 spawn 当场——
    // 上一会话 write_bytes 越界清零页表、spawn 时完好、首次调度才崩，即缺此断言。
    // 守护页必须未映射：一旦被映射，栈溢出防护静默失效（溢出不再触发缺页）。
    debug_assert_eq!(
        space
            .translate(VirtAddr::from_raw(TASK_STACK_BASE))
            .map(|(pa, _)| pa.as_usize()),
        Some(stack_pa),
        "spawn: stack base {TASK_STACK_BASE:#x} not mapped to expected PA {stack_pa:#x}",
    );
    debug_assert!(
        space
            .translate(VirtAddr::from_raw(TASK_STACK_BASE + STACK_SIZE - 1))
            .is_some(),
        "spawn: stack top page not mapped",
    );
    debug_assert!(
        space
            .translate(VirtAddr::from_raw(
                TASK_STACK_BASE - crate::memory::PAGE_SIZE
            ))
            .is_none(),
        "spawn: guard page [BASE-4K, BASE) unexpectedly mapped — stack guard compromised",
    );

    // ④ 栈顶对齐（TASK_STACK_BASE + STACK_SIZE 本就 16 字节对齐），TrapFrame
    //    用物理地址写：此刻活动空间不映射 TASK_STACK_BASE（kernel 空间或其它
    //    任务空间），但所有物理 DRAM 恒为 identity 映射，frame_pa 到处可写。
    let frame_va = (TASK_STACK_BASE + STACK_SIZE - size_of::<TrapFrame>()) as *mut TrapFrame;
    let frame_pa = (stack_pa + STACK_SIZE - size_of::<TrapFrame>()) as *mut TrapFrame;
    unsafe {
        // 清零整个 TrapFrame：首次 dispatch 时 trap_vector 会加载全部寄存器。
        // 注意 write_bytes 按元素计数——必须 cast 成 *mut u8 才按字节写，
        // 否则 count=264 会写 264×264 字节，越过栈顶砸坏相邻页表。
        core::ptr::write_bytes(frame_pa as *mut u8, 0u8, size_of::<TrapFrame>());

        // sp 字段：trap_vector 恢复后的原始栈指针（栈顶 VA）
        (*frame_pa).sp = TASK_STACK_BASE + STACK_SIZE;
        // sepc：任务入口地址
        (*frame_pa).sepc = entry as usize;
        // sstatus：SPP=Supervisor, SPIE=1（sret 后中断使能）
        (*frame_pa).sstatus = (crate::hal::csr::sstatus::Sstatus::SPIE
            | crate::hal::csr::sstatus::Sstatus::SPP)
            .bits();
    }

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let task = Task {
        id,
        state: TaskState::Ready,
        kind,
        frame: frame_va, // VA：调度器返回给 trap_vector 时该任务空间已激活
        space: Some(space),
        stack: stack_pa as *mut u8, // 物理基址：zombie 回收 / frame_phys 用
        stack_size: STACK_SIZE,
        wake_tick: 0,
        resume_sepc: 0,
    };

    let mut q = TASK_QUEUE.lock();
    info!(
        "spawn task id={id} kind={kind:?} entry={entry:p} frame={frame_va:?} stack={stack_pa:#x}"
    );
    q.push_back(task);
}

/// 当前是否运行在"UMode 任务"上下文（语义：异常处置策略标签，非真 U-mode）。
///
/// 决定同步异常是终止任务（[`terminate_current`]）还是内核 panic：
/// UMode 任务 → true；SMode 任务 / 空闲任务 / boot（CURRENT=None）→ false。
pub(crate) fn current_is_umode() -> bool {
    CURRENT
        .lock()
        .as_ref()
        .is_some_and(|c| c.kind == TaskKind::UMode)
}
