// 任务创建（scheduler 子模块）
//
// spawn/spawn_with 创建任务：从 frame 分配器申请栈帧、新建/沿用地址空间、
// 把栈映射到固定虚拟窗口（下方一页守护页留空）、在栈顶构造初始 TrapFrame，
// 最后推入就绪队列。入口类型暂为 fn()（内核代码段指针），ELF 用户程序
// 加载后需按虚拟地址入口另设 API。

use alloc::boxed::Box;
use core::alloc::Layout;
use core::mem::size_of;
use core::sync::atomic::Ordering;

use crate::{
    context::TrapFrame,
    info,
    memory::{
        addr::{PhysAddr, VirtAddr},
        allocator::{frame, page},
        entry::PteFlags,
        space::AddressSpace,
    },
};

use super::task::{NEXT_ID, TASK_QUEUE, Task, TaskKind, TaskState};

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

/// 创建任务：从 frame 分配器申请栈帧，映射到固定虚拟窗口 [`crate::memory::TASK_STACK_BASE`]，
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
        .allocate(
            Layout::from_size_align(crate::memory::TASK_STACK_SIZE, crate::memory::PAGE_SIZE)
                .unwrap(),
        )
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
            VirtAddr::from_raw(crate::memory::TASK_STACK_BASE),
            PhysAddr::from_raw(stack_pa),
            crate::memory::TASK_STACK_SIZE,
            PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D,
            page::allocator(),
        )
        .expect("spawn: map task stack failed");

    // ③后 映射校验（debug 构建）。把"映射是否生效"从首次调度提前到 spawn 当场——
    // 上一会话 write_bytes 越界清零页表、spawn 时完好、首次调度才崩，即缺此断言。
    // 守护页必须未映射：一旦被映射，栈溢出防护静默失效（溢出不再触发缺页）。
    debug_assert_eq!(
        space
            .translate(VirtAddr::from_raw(crate::memory::TASK_STACK_BASE))
            .map(|(pa, _)| pa.as_usize()),
        Some(stack_pa),
        "spawn: stack base {:#x} not mapped to expected PA {stack_pa:#x}",
        crate::memory::TASK_STACK_BASE,
    );
    debug_assert!(
        space
            .translate(VirtAddr::from_raw(
                crate::memory::TASK_STACK_BASE + crate::memory::TASK_STACK_SIZE - 1
            ))
            .is_some(),
        "spawn: stack top page not mapped",
    );
    debug_assert!(
        space
            .translate(VirtAddr::from_raw(
                crate::memory::TASK_STACK_BASE - crate::memory::PAGE_SIZE
            ))
            .is_none(),
        "spawn: guard page [BASE-4K, BASE) unexpectedly mapped — stack guard compromised",
    );

    // ④ 栈顶对齐（crate::memory::TASK_STACK_BASE + crate::memory::TASK_STACK_SIZE 本就 16 字节对齐），TrapFrame
    //    用物理地址写：此刻活动空间不映射 crate::memory::TASK_STACK_BASE（kernel 空间或其它
    //    任务空间），但所有物理 DRAM 恒为 identity 映射，frame_pa 到处可写。
    let frame_va = (crate::memory::TASK_STACK_BASE + crate::memory::TASK_STACK_SIZE
        - size_of::<TrapFrame>()) as *mut TrapFrame;
    let frame_pa =
        (stack_pa + crate::memory::TASK_STACK_SIZE - size_of::<TrapFrame>()) as *mut TrapFrame;
    unsafe {
        // 清零整个 TrapFrame：首次 dispatch 时 trap_vector 会加载全部寄存器。
        // 注意 write_bytes 按元素计数——必须 cast 成 *mut u8 才按字节写，
        // 否则 count=264 会写 264×264 字节，越过栈顶砸坏相邻页表。
        core::ptr::write_bytes(frame_pa as *mut u8, 0u8, size_of::<TrapFrame>());

        // sp 字段：trap_vector 恢复后的原始栈指针（栈顶 VA）
        (*frame_pa).sp = crate::memory::TASK_STACK_BASE + crate::memory::TASK_STACK_SIZE;
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
        stack_size: crate::memory::TASK_STACK_SIZE,
        wake_tick: 0,
        resume_sepc: 0,
    };

    let mut q = TASK_QUEUE.lock();
    info!(
        "spawn task id={id} kind={kind:?} entry={entry:p} frame={frame_va:?} stack={stack_pa:#x}"
    );
    q.push_back(task);
}
