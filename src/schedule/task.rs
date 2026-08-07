// 任务数据结构与全局调度状态 — scheduler 子模块
//
// 存放 Task/TaskKind/TaskState/Pending 定义、调度状态门面 TaskTable（就绪/
// 睡眠/僵尸三个队列与 CURRENT 聚合，任务以 Box<Task> 存于队列——地址稳定
// 的句柄）与 NEXT_ID 全局状态，以及从 CURRENT 推导的查询函数（current_space /
// current_id / current_is_umode）。其余子模块（schedule/sleep/spawn/exit）
// 一律经 TaskTable 方法访问任务数据与队列，不直接触碰锁。
//
// 状态建模：TaskState 是任务的**真实状态**，与容器归属一一对应；Running 任务
// 的"下次 Tick 处置"单独编码在 Pending 字段——见两个枚举的文档。

use alloc::boxed::Box;
use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::sync::atomic::AtomicUsize;

use crate::{context::TrapFrame, lock::SpinLock, memory::space::AddressSpace};

/// 任务真实状态。状态与容器归属一一对应：
///
/// | 状态    | 位置        |
/// |---------|-------------|
/// | Ready   | TASK_QUEUE  |
/// | Blocked | SLEEP_LIST  |
/// | Zombie  | ZOMBIE_LIST |
/// | Running | CURRENT     |
/// | Idle    | 回退任务（队列空时；全空 → 关机 / 否则 wfi 等待，见 schedule.rs） |
///
/// 不再由归属隐含 Running：CURRENT 里的任务 state 恒为 Running（或回退的
/// Idle），其 tick 处置见 [`Pending`]。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TaskState {
    /// 在 TASK_QUEUE，等待被调度
    Ready,
    /// 在 SLEEP_LIST，`wake_tick` 到期前不被调度
    Blocked,
    /// 在 ZOMBIE_LIST，等待下一次 scheduler() 开头回收
    Zombie,
    /// 在 CURRENT，正在执行
    Running,
    /// 回退任务；就绪队列空时创建：所有调度列表全空 → 关机，否则 wfi 等待
    Idle,
}

/// Running 任务在下次 Tick 时的处置（仅 `state == Running` 时读取）。
///
/// sleep()/exit()/wait() 只改此字段、不改 state——任务仍在 CURRENT 运行，
/// 到 Tick 时 scheduler 才按它迁移到目标队列。队列中的任务此字段恒为
/// Rerun（wake_task 复位），不参与迁移。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pending {
    /// 普通运行：Tick → 重排进就绪队列
    Rerun,
    /// 已请求 sleep：Tick → 到期即醒入队，否则进睡眠队列
    Park,
    /// 已请求 wait(pid)：Tick → 子已退出则收尸唤醒，否则进睡眠队列等事件
    Wait(usize),
    /// 已请求输入阻塞读：Tick → 进睡眠队列等输入事件（wake_tick=MAX，
    /// 由 wake_input_waiters 在字符到达时唤醒；UMode 任务唤醒后重放 ecall）
    WaitRead,
    /// 已请求 exit：Tick → 进僵尸队列待回收
    Reap,
}

/// 父任务 wait(pid) 的唤醒结果（仅 `pending == Wait(pid)` 期间由 scheduler /
/// kill 写入，wait() 恢复段读取）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WaitResult {
    /// 尚未就绪（默认值；任务未被唤醒时恒为 Pending）
    Pending,
    /// 目标任务正常退出：携带退出码（exit(code) / 自然返回 0 / 异常终止 -1）
    Exited(i32),
    /// 目标任务被 kill（无退出码，wait 返回 None）。kill 路径见 kill.rs。
    Killed,
}

/// 任务属性：决定运行模式与同步异常（缺页/非法指令等）的处置方向。
///
/// 与 [`crate::schedule::Entry`] 一一对应：SMode = Kernel 任务（S-mode 运行），
/// UMode = User 任务（真 U-mode 运行——spawn 初始帧不置 SPP，sret 后进入 U-mode）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TaskKind {
    /// SMode 任务（内核）：S-mode 运行，同步异常 → panic（内核 bug，崩溃可诊断）
    SMode,
    /// UMode 任务（用户）：真 U-mode 运行，同步异常 → terminate_current（等效 SIGSEGV，系统继续）
    UMode,
}

/// 调度队列条目，经 `Box<Task>` 存于 [`TaskTable`] 队列（地址稳定）。
///
/// 不实现 `Clone`：`space` 持有独占所有权（Box），克隆会共享页表根导致
/// double-free。队列操作全部 move（搬移的是 Box 指针）；空闲快照副本在
/// `scheduler()` 内重建。
pub(crate) struct Task {
    pub(crate) id: usize,
    /// 父任务 id（spawn 调用方的当前任务）；boot 阶段创建的孤儿任务为 None。
    /// 僵尸延迟回收与孤儿收养用（见 schedule.rs / wait.rs）。
    pub(crate) parent: Option<usize>,
    pub(crate) state: TaskState,
    /// 仅 `state == Running` 有意义：下次 Tick 的处置（见 [`Pending`]）
    pub(crate) pending: Pending,
    pub(crate) kind: TaskKind,
    /// 任务 TrapFrame 指针（恒非空：spawn 的栈顶帧 / idle 的合成帧）。
    ///
    /// `terminate_current` 的 null 帧（栈破坏路径）不写入本字段——null 帧
    /// 仅标记 Zombie 并 dispatch，见 `scheduler()` 里的 `NonNull::new` 转接。
    pub(crate) frame: NonNull<TrapFrame>,
    /// 所属地址空间；None = 内核空间（KERNEL_SPACE，仅空闲/boot 任务）。
    ///
    /// 任务**独占**空间所有权（spawn 创建 / 调用方传入，Box 类型系统
    /// 强制一空间一任务）——Zombie 回收时 drop，触发页表树归还 page 分配器。
    pub(crate) space: Option<Box<AddressSpace>>,
    /// 栈物理基址，zombie 回收与 `frame_phys` 用；None = boot 任务不在堆上
    /// （空闲任务的栈在 boot 栈，无独立堆分配）。
    pub(crate) stack: Option<NonNull<u8>>,
    pub(crate) stack_size: usize,
    /// Blocked 时的唤醒时间（mtime 刻度）；wait 阻塞任务恒为 `u64::MAX`
    /// （事件唤醒，时间维度永不过期）
    pub(crate) wake_tick: u64,
    /// 唤醒后的恢复点（sepc 重置目标）— 见 `sleep()` / `wait()`
    pub(crate) resume_sepc: usize,
    /// 退出码（仅 Reap 处置 / 收尸时读取）：exit(code) → Some(code)，
    /// 自然返回 → 0，UMode 异常终止 → -1；未退出任务为 None。
    pub(crate) exit_code: Option<i32>,
    /// 正在等待的任务 id（仅 `pending == Wait(pid)` 期间有意义）：父任务
    /// 阻塞等待的目标，子退出时 scheduler 据此唤醒父。
    pub(crate) wait_pid: Option<usize>,
    /// wait 唤醒结果（见 [`WaitResult`]）；scheduler/kill 写入，wait() 恢复段读取。
    pub(crate) wait_result: WaitResult,
}

/// 调度状态门面：就绪/睡眠/僵尸三个队列与 CURRENT 聚合在一个结构体内，由
/// 单一 `TASK_TABLE` 锁保护——跨队列操作（kill 查三队、wait 收尸、延迟回收
/// 判活）不再需要手工编排多锁顺序。
///
/// 任务以 `Box<Task>` 存于队列：Task 本体地址固定（稳定句柄），队列间传递只
/// 搬 8 字节指针；Task 仍不实现 `Clone`（`space` 独占所有权，见 [`Task`]）。
pub(crate) struct TaskTable {
    /// 就绪队列（Round-robin）。
    ready: VecDeque<Box<Task>>,
    /// 睡眠队列 — Blocked 任务，到期后由 `pop_due_sleepers` 移回就绪队列。
    sleep: VecDeque<Box<Task>>,
    /// 僵尸队列 — 已退出任务，等待回收（或父任务 wait 收尸）。
    zombie: VecDeque<Box<Task>>,
    /// 当前运行任务（state=Running，或空闲回退的 state=Idle）；缺页处理器经
    /// `current_space()` 由它推导活动地址空间。
    ///
    /// TODO: 多 hart 时应改为 per-hart 数组 [SpinLock<Option<Task>>; MAX_HARTS]，
    ///       按 hart_id 索引，避免不同 hart 的 CURRENT 互相覆盖。
    current: Option<Box<Task>>,
}

/// 全部调度状态（单锁）。
pub(crate) static TASK_TABLE: SpinLock<TaskTable> = SpinLock::new(TaskTable::new());

impl TaskTable {
    pub(crate) const fn new() -> Self {
        Self {
            ready: VecDeque::new(),
            sleep: VecDeque::new(),
            zombie: VecDeque::new(),
            current: None,
        }
    }

    /// 就绪队列：队尾入队。
    pub(crate) fn push_ready(&mut self, t: Box<Task>) {
        self.ready.push_back(t);
    }

    /// 就绪队列：队首出队（round-robin 选中下一个运行任务）。
    pub(crate) fn pop_ready(&mut self) -> Option<Box<Task>> {
        self.ready.pop_front()
    }

    /// 睡眠队列：入队（Blocked 任务）。
    pub(crate) fn push_sleep(&mut self, t: Box<Task>) {
        self.sleep.push_back(t);
    }

    /// 睡眠队列：把到期（`wake_tick <= now`）任务移入 `due`，未到期者留在队内。
    /// wait 阻塞任务 `wake_tick = u64::MAX`（事件唤醒），天然永不在此被移出。
    ///
    /// `due` 元素必须是 `Box<Task>`：队列以 Box 稳定句柄存任务（地址固定、
    /// 传递只搬指针），到期任务带着 Box 移出后入就绪队列。clippy::vec_box
    /// 在此是误报——Box 是句柄语义而非「Vec 已上堆所以多余」。
    #[allow(clippy::vec_box)]
    pub(crate) fn pop_due_sleepers(&mut self, now: u64, due: &mut Vec<Box<Task>>) {
        let mut pending = Vec::new();
        for t in self.sleep.drain(..) {
            if t.wake_tick <= now {
                due.push(t);
            } else {
                pending.push(t);
            }
        }
        for t in pending {
            self.sleep.push_back(t);
        }
    }

    /// 僵尸队列：入队（已退出任务，等待回收或父收尸）。
    pub(crate) fn push_zombie(&mut self, t: Box<Task>) {
        self.zombie.push_back(t);
    }

    /// 僵尸队列：整体移出（调度器开头批量回收）。
    pub(crate) fn take_zombies(&mut self) -> VecDeque<Box<Task>> {
        core::mem::take(&mut self.zombie)
    }

    /// 僵尸队列：按 id 移出第一个匹配任务（wait 收尸用），其余原序保留。
    pub(crate) fn take_zombie(&mut self, id: usize) -> Option<Box<Task>> {
        take_first(&mut self.zombie, |t| t.id == id)
    }

    /// 睡眠队列：移出正在等 `child_id` 的任务（Reap 处置时唤醒父收尸用）。
    pub(crate) fn take_sleeper_waiting(&mut self, child_id: usize) -> Option<Box<Task>> {
        take_first(&mut self.sleep, |t| t.wait_pid == Some(child_id))
    }

    /// 睡眠队列：移出全部输入等待任务（`wake_input_waiters` 用），其余原序保留。
    ///
    /// 返回 `Vec<Box<Task>>`：队列以 Box 稳定句柄存任务（地址固定、传递只搬
    /// 指针），唤醒时带着 Box 入就绪队列。clippy::vec_box 在此是误报——Box
    /// 是句柄语义而非「Vec 已上堆所以多余」（同 [`Self::pop_due_sleepers`]）。
    #[allow(clippy::vec_box)]
    pub(crate) fn take_input_waiters(&mut self) -> Vec<Box<Task>> {
        let mut waiting = Vec::new();
        let mut pending = Vec::new();
        for t in self.sleep.drain(..) {
            if t.pending == Pending::WaitRead {
                waiting.push(t);
            } else {
                pending.push(t);
            }
        }
        for t in pending {
            self.sleep.push_back(t);
        }
        waiting
    }

    /// 任一调度队列（就绪/睡眠/僵尸）按 id 移出第一个匹配任务（kill 用；
    /// **不含 CURRENT**——运行中的任务不可他杀，只能自行 exit）。其余原序保留。
    pub(crate) fn take(&mut self, id: usize) -> Option<Box<Task>> {
        if let Some(t) = take_first(&mut self.ready, |t| t.id == id) {
            return Some(t);
        }
        if let Some(t) = take_first(&mut self.sleep, |t| t.id == id) {
            return Some(t);
        }
        take_first(&mut self.zombie, |t| t.id == id)
    }

    /// 当前任务（只读借用，`&Task` 而非 `&Box<Task>`——调用方只读字段）。
    pub(crate) fn current(&self) -> Option<&Task> {
        self.current.as_deref()
    }

    /// 当前任务（可变借用）。
    pub(crate) fn current_mut(&mut self) -> Option<&mut Box<Task>> {
        self.current.as_mut()
    }

    /// 当前任务：取出（调度器处置用）。
    pub(crate) fn take_current(&mut self) -> Option<Box<Task>> {
        self.current.take()
    }

    /// 当前任务：放入（调度器 dispatch 用）。
    pub(crate) fn replace_current(&mut self, t: Box<Task>) {
        self.current = Some(t);
    }

    /// 三个调度列表（就绪/睡眠/僵尸）是否全空——全空意味着系统再无工作，
    /// 调度器据此决定关机（而非 wfi 空转等 tick）。
    pub(crate) fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.sleep.is_empty() && self.zombie.is_empty()
    }

    /// 任务是否存活（就绪/睡眠/当前任务中存在该 id；**不含僵尸**——父子
    /// 双僵尸时互不吊住，由延迟回收各自释放）。
    pub(crate) fn alive(&self, id: usize) -> bool {
        self.current.as_ref().is_some_and(|c| c.id == id)
            || self.ready.iter().any(|t| t.id == id)
            || self.sleep.iter().any(|t| t.id == id)
    }
}

/// 从队列中按谓词移出第一个匹配任务（其余原序保留）。
fn take_first(q: &mut VecDeque<Box<Task>>, pred: impl Fn(&Task) -> bool) -> Option<Box<Task>> {
    let mut found = None;
    let mut rest = VecDeque::new();
    for t in q.drain(..) {
        if found.is_none() && pred(&t) {
            found = Some(t);
        } else {
            rest.push_back(t);
        }
    }
    for t in rest {
        q.push_back(t);
    }
    found
}

/// 任务 id 分配器。
pub(crate) static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// 查询当前活动地址空间：从 [`TASK_TABLE`] 的 current 推导，None = 内核空间。
///
/// 缺页处理器用它在内核空间与用户空间之间路由。调度器在 dispatch 时切换
/// 地址空间并更新 current，这里始终反映"正在运行的任务"的空间。
///
/// 返回 `NonNull`：借用无法脱离锁的临时生命周期。current 持有空间所有权
/// （Box），堆数据地址稳定、锁释放后仍有效，缺页处理器在 trap 上下文使用。
pub fn current_space() -> Option<NonNull<AddressSpace>> {
    TASK_TABLE
        .lock()
        .current()
        .and_then(|c| c.space.as_deref().map(NonNull::from))
}

/// 当前任务 id（无任务时返回 `usize::MAX`）。
pub(crate) fn current_id() -> usize {
    TASK_TABLE
        .lock()
        .current()
        .map(|c| c.id)
        .unwrap_or(usize::MAX)
}

/// 当前是否运行在"UMode 任务"上下文（真 U-mode：spawn 初始帧 SPP=0，sret 后 U-mode）。
///
/// 决定同步异常是终止任务（`terminate_current`）还是内核 panic：
/// UMode 任务 → true；SMode 任务 / 空闲任务 / boot（current=None）→ false。
pub(crate) fn current_is_umode() -> bool {
    TASK_TABLE
        .lock()
        .current()
        .is_some_and(|c| c.kind == TaskKind::UMode)
}
