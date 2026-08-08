// 任务创建（scheduler 子模块）
//
// spawn 是唯一任务创建入口，按 [`Entry`] 区分任务形态：
//   Kernel(fn()) — 内核任务：S-mode 运行，同步异常 → panic
//   User(VirtAddr) — 用户任务：真 U-mode 运行，同步异常 → terminate_current
// 创建过程：从 frame 分配器申请栈帧、新建/沿用地址空间、把栈映射到固定虚拟
// 窗口（下方一页守护页留空）、在栈顶构造初始 TrapFrame，最后推入就绪队列。
// 初始帧 ra 指向入口返回 trampoline（[`task_entry_return`]）——入口自然 return
// 时干净退出，而非取指 0x0 触发缺页故障。用户任务入口为用户空间虚拟地址
// （代码页的 U|R|X 映射由调用方负责，ELF loader 落地的同一路径）。

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::alloc::Layout;
use core::mem::size_of;
use core::ptr::NonNull;
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

use super::exit;
use super::task::{
    NEXT_ID, Pending, TASK_TABLE, Task, TaskKind, TaskState, WaitResult, current_id,
};

/// 任务入口——统一 [`spawn`] 的入口形态与隐含语义。
///
/// 两种形态对应两种任务：入口类型由编译期区分（fn 指针保证指向内核镜像；
/// VirtAddr 携带用户空间虚拟地址），运行模式与异常处置策略由 [`TaskKind`] 承载。
pub enum Entry {
    /// 内核任务：S-mode 运行，同步异常 → panic（内核 bug，崩溃可诊断）。
    Kernel(fn()),
    /// 用户任务：真 U-mode 运行，同步异常 → terminate_current（等效 SIGSEGV）。
    ///
    /// 入口 = 用户空间虚拟地址，须指向已映射 U|R|X 的代码页（映射由调用方
    /// 负责，见 [`spawn`] 的说明）；须显式提供地址空间（`space: Some`）。
    User(VirtAddr),
}

/// 任务入口自然返回后的落点（ret_from_fork 模式）。
///
/// [`TaskBuilder::spawn`] 把初始帧的 `ra` 设为本函数：任务入口函数不再要求"永不
/// 返回"——自然 return 时 `ret` 跳到这里，以 code=0 干净退出（等效
/// "main 返回 → exit_group(0)"）。调用 `exit(0)` 时编译器负责把 0 载入 a0
/// （RISC-V 第一个整数参数寄存器），杜绝 entry 残留值污染退出码。
///
/// 取代旧的 ra=0 语义（自然返回 → 取指 0x0 → 缺页故障：SMode panic /
/// UMode SIGSEGV）。
fn task_entry_return() -> ! {
    exit(0)
}

/// 任务空间规格 — 独立（Owned）或线程共享（Shared）。
enum SpaceRef {
    /// 独立空间：Some = 调用方显式提供（所有权移交任务）；None = 自动克隆。
    Owned(Option<Box<AddressSpace>>),
    /// 线程共享空间：Arc 引用计数，与组长共享页表（见 [`TaskBuilder::shared`]）。
    Shared(Arc<AddressSpace>),
}

/// 任务构造器 — 集中任务规格（入口/地址空间/优先级）后统一 `spawn()`，
/// 取代 spawn 参数列表随功能增长的膨胀。用法：
///
/// ```ignore
/// TaskBuilder::new(Entry::Kernel(task_a)).spawn();              // 默认优先级
/// TaskBuilder::new(Entry::Kernel(task_a)).priority(0).spawn();  // 高优先级
/// TaskBuilder::new(entry).space(space).spawn();                 // 指定地址空间
/// TaskBuilder::new(Entry::Kernel(thread)).shared(arc).spawn();  // 线程（共享空间）
/// ```
pub struct TaskBuilder {
    entry: Entry,
    space: SpaceRef,
    priority: u8,
}

impl TaskBuilder {
    /// 新建构造器 — 必填任务入口（见 [`Entry`]）。
    pub fn new(entry: Entry) -> Self {
        Self {
            entry,
            space: SpaceRef::Owned(None),
            priority: 128, // 默认优先级 → 8 tick（time_slice 中点）
        }
    }

    /// 指定任务地址空间（缺省 None = 自动 `from_kernel` 克隆；User 任务必填）。
    pub fn space(mut self, space: Option<Box<AddressSpace>>) -> Self {
        self.space = SpaceRef::Owned(space);
        self
    }

    /// 线程形态：与已有任务共享地址空间（`Arc` 引用计数——最后一个持有者
    /// 退出才释放页表树）。共享页表 = 共享内存，线程间经共享变量传参；
    /// 栈窗口动态分配错开（同空间多线程互不覆盖）。
    pub fn shared(mut self, space: Arc<AddressSpace>) -> Self {
        self.space = SpaceRef::Shared(space);
        self
    }

    /// 指定调度优先级 — 小 = 高（0-255，缺省 128）。见 [`super::task::time_slice`]。
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// 从 ELF 镜像装载用户程序（loader 静态工厂）。
    ///
    /// `blob` 为无 libc 的 RISC-V 静态 ELF（ET_EXEC）：解析 + 逐段映射进新
    /// 地址空间（代码 R|X、数据 R|W、.bss 零页），返回已就绪的构造器——
    /// `TaskBuilder::loader(blob)?.spawn()` 一条龙。不动 [`Entry`] 枚举。
    ///
    /// # Errors
    ///
    /// 失败（格式非法 / 物理帧耗尽 / 段重叠）返回 [`crate::loader::LoadError`]，
    /// 不创建任务。
    pub fn loader(blob: &[u8]) -> Result<Self, crate::loader::LoadError> {
        let (space, entry) = crate::loader::load(blob)?;
        Ok(Self::new(Entry::User(entry)).space(Some(space)))
    }

    /// 创建任务：从 frame 分配器申请栈帧，映射到固定虚拟窗口
    /// [`crate::memory::TASK_STACK_BASE`]，在栈顶构造初始 [`TrapFrame`]，
    /// 推入调度队列——下一次定时器中断发生时，调度器会选中它。
    ///
    /// `entry` 可自然返回：初始帧 ra 指向 [`task_entry_return`]，入口 return
    /// 时干净退出（code=0）；也可显式调用 [`exit()`] 带退出码，或 `loop` 永续。
    ///
    /// UMode 任务：栈映射追加 U 位（U-mode 可读写；S-mode 侧写该栈依赖
    /// sstatus.SUM——trap_vector 入口置位，见 runtime/trap.rs），初始帧 sstatus
    /// 不置 SPP（sret 后进入 U-mode，中断使能）。
    ///
    /// 返回新任务 id（单调递增，可作 wait/kill 句柄）。
    pub fn spawn(self) -> usize {
        let (kind, entry_addr) = match self.entry {
            Entry::Kernel(f) => (TaskKind::SMode, f as usize),
            Entry::User(va) => {
                assert!(
                    matches!(&self.space, SpaceRef::Owned(Some(_)) | SpaceRef::Shared(_)),
                    "Entry::User requires an explicit address space (auto-clone not allowed)"
                );
                assert!(
                    va.is_user(),
                    "Entry::User entry {va:?} is not a user-space address"
                );
                (TaskKind::UMode, va.as_usize())
            }
        };
        //   从 frame 分配器申请 16 KiB 物理栈帧（order-2，页对齐）
        let stack = frame::allocator()
            .allocate(
                Layout::from_size_align(crate::memory::TASK_STACK_SIZE, crate::memory::PAGE_SIZE)
                    .unwrap(),
            )
            .expect("spawn: stack allocation failed");
        let stack_pa = stack.as_ptr() as *mut u8 as usize; // 物理基址（瘦化胖指针）

        // 确定任务空间：Owned(Some) 沿用调用方空间；Owned(None) 自动克隆；
        // Shared 共享已有空间（线程形态，Arc 引用计数——回收时计数归零才释放）。
        let space: Arc<AddressSpace> = match self.space {
            SpaceRef::Owned(Some(s)) => Arc::new(*s), // 解 Box：唯一所有权移交
            SpaceRef::Owned(None) => Arc::new(
                AddressSpace::from_kernel(page::allocator())
                    .expect("spawn: clone kernel space failed"),
            ),
            SpaceRef::Shared(s) => s,
        };
        // 线程栈窗口：窗口号单调分配（独立任务新空间从 0 起 = 现状 BASE；
        // 线程从组长已用窗口续）。窗口 VA 带独立守护页（窗口间无缝）。
        let slot = space.stack_windows.fetch_add(1, Ordering::Relaxed);
        let stack_va = space.stack_window_va(slot);

        // 把栈映射到窗口 VA（动态分配）；守护页 [stack_va-4K, stack_va) 不映射
        // （纯虚拟留空）。
        // 不带 G：switch_space 的 sfence.vma 以通用寄存器传本任务 ASID（rs2≠x0），
        // 只刷该 ASID 的非全局条目——带 G 的栈条目不随 ASID 刷新，跨任务切换
        // 会残留（同 VA 命中上一任务的物理帧）。也无 X（栈不可执行）。
        // UMode 任务追加 U 位：用户任务需能在 U-mode 读写自己的栈；S-mode 侧
        // （trap 保存帧 / 调度器读帧）写该栈依赖 sstatus.SUM（trap_vector 入口置位）。
        let mut stack_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
        if kind == TaskKind::UMode {
            stack_flags |= PteFlags::U;
        }
        space
            .map(
                VirtAddr::from_raw(stack_va),
                PhysAddr::from_raw(stack_pa),
                crate::memory::TASK_STACK_SIZE,
                stack_flags,
                page::allocator(),
            )
            .expect("spawn: map task stack failed");

        // 映射校验（debug 构建）。把"映射是否生效"从首次调度提前到 spawn 当场——
        // 上一会话 write_bytes 越界清零页表、spawn 时完好、首次调度才崩，即缺此断言。
        // 守护页必须未映射：一旦被映射，栈溢出防护静默失效（溢出不再触发缺页）。
        debug_assert_eq!(
            space
                .translate(VirtAddr::from_raw(stack_va))
                .map(|(pa, _)| pa.as_usize()),
            Some(stack_pa),
            "spawn: stack base {stack_va:#x} not mapped to expected PA {stack_pa:#x}",
        );
        debug_assert!(
            space
                .translate(VirtAddr::from_raw(
                    stack_va + crate::memory::TASK_STACK_SIZE - 1
                ))
                .is_some(),
            "spawn: stack top page not mapped",
        );
        debug_assert!(
            space
                .translate(VirtAddr::from_raw(stack_va - crate::memory::PAGE_SIZE))
                .is_none(),
            "spawn: guard page [va-4K, va) unexpectedly mapped — stack guard compromised",
        );

        // 栈顶对齐（crate::memory::TASK_STACK_BASE + crate::memory::TASK_STACK_SIZE 本就 16 字节对齐），TrapFrame
        // 用物理地址写：此刻活动空间不映射 crate::memory::TASK_STACK_BASE（kernel 空间或其它
        // 任务空间），但所有物理 DRAM 恒为 identity 映射，frame_pa 到处可写。
        let frame_va =
            (stack_va + crate::memory::TASK_STACK_SIZE - size_of::<TrapFrame>()) as *mut TrapFrame;
        let frame_pa =
            (stack_pa + crate::memory::TASK_STACK_SIZE - size_of::<TrapFrame>()) as *mut TrapFrame;
        unsafe {
            // 清零整个 TrapFrame：首次 dispatch 时 trap_vector 会加载全部寄存器。
            // 注意 write_bytes 按元素计数——必须 cast 成 *mut u8 才按字节写，
            // 否则 count=264 会写 264×264 字节，越过栈顶砸坏相邻页表。
            core::ptr::write_bytes(frame_pa as *mut u8, 0u8, size_of::<TrapFrame>());

            // ra：入口自然返回后的落点（ret_from_fork trampoline）——干净退出
            // 而非取指 0x0 缺页。entry 内部分子函数时压栈保存/恢复，返回时
            // ra 恒为初始值，`ret` 恰好跳到 trampoline。
            (*frame_pa).ra = task_entry_return as *const () as usize;
            // sp 字段：trap_vector 恢复后的原始栈指针（栈顶 VA）
            (*frame_pa).sp = stack_va + crate::memory::TASK_STACK_SIZE;
            // sepc：任务入口地址（内核 fn 值 / 用户 VA）
            (*frame_pa).sepc = entry_addr;
            // sstatus：SPIE=1（sret 后中断使能）；SMode 任务 SPP=Supervisor（sret
            // 回 S-mode），UMode 任务不置 SPP（sret 后进入 U-mode）。
            // SUM=1：恢复段在 csrw sstatus（恢复帧值）**之后**仍要读任务栈帧
            // （ld ra/gp/.../sp/t0）——初始帧不含 SUM 会让首次 dispatch 恢复段
            // 缺页（嵌套 trap 覆盖 sepc/sstatus → sret 错乱）。首次 trap 后帧被
            // 覆盖（trap 入口置 SUM 后保存），此处只需喂首次 dispatch。
            (*frame_pa).sstatus = if kind == TaskKind::UMode {
                (crate::hal::csr::sstatus::Sstatus::SPIE | crate::hal::csr::sstatus::Sstatus::SUM)
                    .bits()
            } else {
                (crate::hal::csr::sstatus::Sstatus::SPIE
                    | crate::hal::csr::sstatus::Sstatus::SPP
                    | crate::hal::csr::sstatus::Sstatus::SUM)
                    .bits()
            };
        }

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        // 父任务 id：spawn 调用方所在的当前任务（boot 阶段无任务 → None）
        let parent_id = current_id();
        let parent = (parent_id != usize::MAX).then_some(parent_id);
        let task = Task {
            id,
            parent,
            state: TaskState::Ready,
            pending: Pending::Rerun, // 队列中的任务：正常重排处置
            kind,
            priority: self.priority, // 选中调度时经 time_slice 换算时间片
            ticks_left: 0,
            stack_va,
            frame: NonNull::new(frame_va).unwrap(), // VA：调度器返回给 trap_vector 时该任务空间已激活
            space: Some(space),
            stack: Some(NonNull::new(stack_pa as *mut u8).unwrap()), // 物理基址：zombie 回收 / frame_phys 用
            stack_size: crate::memory::TASK_STACK_SIZE,
            wake_tick: 0,
            resume_sepc: 0,
            exit_code: None,
            wait_pid: None,
            wait_result: WaitResult::Pending,
        };

        info!(
            "spawn task id={id:>#x} parent={:?} kind={kind:?} prio={} entry={entry_addr:#x} stack_va={stack_va:#x} stack={stack_pa:#x} asid={}",
            task.parent,
            task.priority,
            task.space.as_ref().map(|sp| sp.asid()).unwrap_or(0),
        );
        TASK_TABLE.lock().push_ready(Box::new(task));
        id
    }
}
