use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use core::mem::{size_of, MaybeUninit};

use crate::{info, lock::SpinLock, trap::TrapFrame};

static TASK_QUEUE: SpinLock<VecDeque<*mut TrapFrame>> = SpinLock::new(VecDeque::new());

/// 每个内核任务栈的大小
const STACK_SIZE: usize = 4096;

pub fn scheduler(frame: *mut TrapFrame) -> usize {
    TASK_QUEUE.lock(|q| -> usize {
        // 当前被抢占的任务入队
        q.push_back(frame);

        // 从队头取出下一个任务
        if let Some(f) = q.pop_front() {
            info!("switch {frame:?} → {f:?}");
            f as usize
        } else {
            // 队列空：继续执行当前任务
            frame as usize
        }
    })
}

/// 创建一个新的内核任务。
///
/// 从全局分配器申请一块栈内存，在栈顶构造初始 [`TrapFrame`]，
/// 然后将其推入调度队列。下一次定时器中断发生时，调度器会选中它。
///
/// `entry` 是一个永不返回的函数指针（不应包含 `ret` 路径）。
pub fn spawn(entry: fn()) {
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
        // mepc：任务入口地址
        (*frame).mepc = entry as usize;
        // mstatus：MPP=M-mode(3), MPIE=1（mret 后中断使能）
        (*frame).mstatus = (3 << 11) | (1 << 7);
    }

    TASK_QUEUE.lock(|q| {
        info!("spawn task entry={entry:p} frame={frame:?} stack={stack_top:?}");
        q.push_back(frame);
    });
}
