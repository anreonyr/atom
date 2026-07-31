// 任务调度器 — round-robin 就绪队列
//
// 每个任务携带所属地址空间的根页号，调度器在上下文切换时调用
// `switch_page_table` 激活新任务的地址空间。

use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::mem::{size_of, MaybeUninit};

use crate::{info, lock::SpinLock, memory, trap::TrapFrame};

/// 调度队列条目。
#[derive(Clone, Copy)]
pub(crate) struct Task {
    pub frame: *mut TrapFrame,
    pub root_page_number: usize,
}

static TASK_QUEUE: SpinLock<VecDeque<Task>> = SpinLock::new(VecDeque::new());

/// 当前活动地址空间的根页号。
///
/// 由调度器在上下文切换时更新，供缺页处理器确定应查询哪个页表。
/// 当前所有任务共享内核地址空间，此值指向 `KERNEL_SPACE` 的根页表。
// TODO: 多 hart 时应改为 per-hart 数组 [SpinLock<Option<usize>>; MAX_HARTS]，
//       按 hart_id 索引，避免不同 hart 的活动地址空间互相覆盖。
static CURRENT_SPACE: SpinLock<Option<usize>> = SpinLock::new(None);

/// 获取当前活动地址空间的根页号。
pub fn current_root_page_number() -> Option<usize> {
    *CURRENT_SPACE.lock()
}

/// 设置当前活动地址空间的根页号。
pub fn set_current_root_page_number(root: usize) {
    *CURRENT_SPACE.lock() = Some(root);
}

/// 每个内核任务栈的大小
const STACK_SIZE: usize = 4096;

/// 调���器入口 — 时钟中断处理中调用。
///
/// 将当前任务入队，弹出下一任务，切换地址空间，返回新任务的 TrapFrame 指针。
pub fn scheduler(frame: *mut TrapFrame) -> usize {
    let current_root = *CURRENT_SPACE.lock();

    let mut q = TASK_QUEUE.lock();
    q.push_back(Task {
        frame,
        root_page_number: current_root.unwrap_or(0),
    });

    if let Some(next) = q.pop_front() {
        info!("switch {frame:?} → {:?}", next.frame);

        // 切换地址空间到新任务的页表
        *CURRENT_SPACE.lock() = Some(next.root_page_number);
        // SAFETY: 关中断状态，单 hart，新任务的页表应包含代码映射
        // SAFETY: 关中断状态，当前所有任务使用 ASID 0（内核空间）
        unsafe {
            memory::switch_space(next.root_page_number, 0);
        }

        next.frame as usize
    } else {
        // 队列空：继续执行当前任务
        frame as usize
    }
}

/// 创建一个新的内核任务。
///
/// 从全局分配器申请一块栈内存，在栈顶构造初始 [`TrapFrame`]，
/// 然后将其推入调度队列。下一次定时器中断发生时，调度器会选中它。
///
/// `entry` 是一个永不返回的函数指针（不应包含 `ret` 路径）。
///
/// 新任务的 TrapFrame 配置为 sret 后进入 S-mode 且中断使能。
/// 创建 TrapFrame 并推入调度队列，使用指定的地址空间根页号。
fn spawn_impl(entry: fn(), root_page_number: usize) {
    // 从 bump allocator 申请栈内存（MaybeUninit 避免 clippy::uninit_vec）
    let mut stack: Vec<MaybeUninit<u8>> = Vec::with_capacity(STACK_SIZE);
    // SAFETY: MaybeUninit 明确表示内存未初始化；我们只将其用作栈的原始字节空间
    unsafe { stack.set_len(STACK_SIZE) };
    let ptr = stack.as_mut_ptr() as *mut u8;
    core::mem::forget(stack); // 接管所有权，永不释放

    // 栈顶 16 字节对齐（RISC-V ABI 要求）
    let stack_top = ((ptr as usize + STACK_SIZE) & !15) as *mut u8;
    let frame = (stack_top as usize - size_of::<TrapFrame>()) as *mut TrapFrame;

    unsafe {
        // 清零整个 TrapFrame
        core::ptr::write_bytes(frame, 0u8, 1);

        // sp 字段：trap_vector 恢复后的原始栈指针（栈顶）
        (*frame).sp = stack_top as usize;
        // sepc：任务入口地址
        (*frame).sepc = entry as usize;
        // sstatus：SPP=Supervisor, SPIE=1（sret 后中断使能）
        (*frame).sstatus =
            crate::hal::csr::sstatus::Sstatus::SPIE.bits() | crate::hal::csr::sstatus::SPP;
    }

    let mut q = TASK_QUEUE.lock();
    info!("spawn task entry={entry:p} frame={frame:?} stack={stack_top:?}");
    q.push_back(Task {
        frame,
        root_page_number: root_page_number,
    });

    set_current_root_page_number(root_page_number);
}

/// 创建一个新任务（使用 KERNEL_SPACE）。
pub fn spawn(entry: fn()) {
    let root = memory::space::kernel_space()
        .as_ref()
        .map(|ks| ks.root_page() as usize)
        .unwrap_or(0);
    spawn_impl(entry, root);
}

/// 创建一个新任务（指定地址空间根页号）。
pub fn spawn_with(entry: fn(), root_page_number: usize) {
    spawn_impl(entry, root_page_number);
}
