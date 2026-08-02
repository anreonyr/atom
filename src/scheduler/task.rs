// 任务数据结构与全局调度状态 — scheduler 子模块
//
// 存放 Task/TaskState/TaskKind 定义、三个调度队列与 CURRENT/IDLE_TASK/NEXT_ID
// 全局状态，以及从 CURRENT 推导的查询函数（current_space / current_id /
// current_is_umode）。其余子模块（schedule/sleep/spawn/exit）经这些
// pub(crate) 项访问任务数据与队列。

use alloc::boxed::Box;
use alloc::collections::vec_deque::VecDeque;
use core::sync::atomic::AtomicUsize;

use crate::{context::TrapFrame, lock::SpinLock, memory::space::AddressSpace};

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
/// double-free。队列操作全部 move；空闲快照副本在 `scheduler()` 内重建。
pub(crate) struct Task {
    pub(crate) id: usize,
    pub(crate) state: TaskState,
    pub(crate) kind: TaskKind,
    pub(crate) frame: *mut TrapFrame,
    /// 所属地址空间；None = 内核空间（KERNEL_SPACE，仅空闲/boot 任务）。
    ///
    /// 任务**独占**空间所有权（spawn 创建 / spawn_with 传入，Box 类型系统
    /// 强制一空间一任务）——Zombie 回收时 drop，触发页表树归还 page 分配器。
    pub(crate) space: Option<Box<AddressSpace>>,
    /// 栈物理基址，zombie 回收与 `frame_phys` 用（boot 任务不在堆上，为 null）
    pub(crate) stack: *mut u8,
    pub(crate) stack_size: usize,
    /// Blocked 时的唤醒时间（mtime 刻度）
    pub(crate) wake_tick: u64,
    /// 唤醒后的恢复点（sepc 重置目标）— 见 `sleep()`
    pub(crate) resume_sepc: usize,
}

/// 就绪队列（Round-robin）。
pub(crate) static TASK_QUEUE: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 睡眠队列 — Blocked 任务，到期后由 `wake_sleepers` 移回就绪队列。
pub(crate) static SLEEP_LIST: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 僵尸队列 — 已退出任务，下一个 scheduler() 开头回收其栈。
pub(crate) static ZOMBIE_LIST: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 当前运行任务。存在即 running；缺页处理器经 `current_space()` 由它推导活动地址空间。
///
/// TODO: 多 hart 时应改为 per-hart 数组 [SpinLock<Option<Task>>; MAX_HARTS]，
///       按 hart_id 索引，避免不同 hart 的 CURRENT 互相覆盖。
pub(crate) static CURRENT: SpinLock<Option<Task>> = SpinLock::new(None);

/// 空闲任务 — boot 任务（main）首次被抢占时建立的快照。
///
/// 空闲任务永不入就绪队列；就绪队列为空时回退到它（main 的 wfi 空转）。
pub(crate) static IDLE_TASK: SpinLock<Option<Task>> = SpinLock::new(None);

/// 任务 id 分配器。
pub(crate) static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// 查询当前活动地址空间：从 [`CURRENT`] 推导，None = 内核空间。
///
/// 缺页处理器用它在内核空间与用户空间之间路由。调度器在 dispatch 时切换
/// 地址空间并更新 CURRENT，这里始终反映"正在运行的任务"的空间。
///
/// 返回裸指针：借用无法脱离锁的临时生命周期。`CURRENT` 持有空间所有权
/// （Box），堆数据地址稳定、锁释放后仍有效，缺页处理器在 trap 上下文使用。
pub fn current_space() -> Option<*const AddressSpace> {
    CURRENT
        .lock()
        .as_ref()
        .and_then(|c| c.space.as_deref().map(|s| s as *const AddressSpace))
}

/// 当前任务 id（无任务时返回 `usize::MAX`）。
pub(crate) fn current_id() -> usize {
    CURRENT.lock().as_ref().map(|c| c.id).unwrap_or(usize::MAX)
}

/// 当前是否运行在"UMode 任务"上下文（语义：异常处置策略标签，非真 U-mode）。
///
/// 决定同步异常是终止任务（`terminate_current`）还是内核 panic：
/// UMode 任务 → true；SMode 任务 / 空闲任务 / boot（CURRENT=None）→ false。
pub(crate) fn current_is_umode() -> bool {
    CURRENT
        .lock()
        .as_ref()
        .is_some_and(|c| c.kind == TaskKind::UMode)
}
