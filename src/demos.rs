// 演示任务集 — 从 main.rs 抽离的 demo 代码（评审 C7）
//
// 每个 demo 一个编译期开关（DEMO_*）+ 一个 demo_* 入口 + 若干任务函数。
// 复现某个场景时把对应开关置 true、其余保持 false——一次只跑一个 demo，
// 日志不被多个任务互相淹没。`run()` 在 main 中调用；基础任务 `task` 由 main 恒跑。

use crate::filesystem;
use crate::memory::addr::{PhysAddr, VirtAddr};
use crate::memory::allocator::{frame, page};
use crate::memory::entry::PteFlags;
use crate::memory::space::{AddressSpace, RegionKind};
use crate::scheduler;
use alloc::boxed::Box;
use core::alloc::Layout;
use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

// ── U-mode 测试代码段 ──────────────────────────────────────────
//
// 编译期汇编（global_asm），链接进内核镜像 .text（link.ld 的 *(.text*)
// 捕获 .text.user_code 子段）；运行时由 [`map_user_code`] 拷入用户页执行。
// 约束：仅用寄存器 + 立即数 + 相对分支（位置无关 PIC），不访问任何全局
// 地址——拷到用户空间后独立可运行。ecall 返回 -ENOSYS 校验：两次 ecall
// 都进入 envcall 分发（日志可见）即证明返回值正确写回 + sepc 跳过。
global_asm!(
    ".section .text.user_code, \"ax\"",
    // ecall 闭环：未知号 ×2（期望 -ENOSYS）→ 非法指令 → terminate
    ".globl _u_ecall_test",
    "_u_ecall_test:",
    "  li  a7, 0xED",        // 未知调用号
    "  li  a0, 0x1234",
    "  ecall",               // → trap_handler scause=8 → envcall::dispatch
    "  li  t0, -38",         // 期望返回值 = -ENOSYS（两补）
    "  bne a0, t0, 2f",      // 校验失败也走非法指令 terminate（日志可辨）
    "  li  a7, 0xEE",        // 第二次 ecall：验证 sepc+4 跳过、可重复
    "  li  a0, 0x5678",
    "  ecall",
    "  bne a0, t0, 2f",
    "  .word 0",             // 非法指令（0x00000000）→ 同步异常 → terminate
    "2:",
    "  .word 0",
    ".globl _u_ecall_test_end",
    "_u_ecall_test_end:",

    // 未映射访问：load 0x7E00_0000 → 缺页（scause=13）→ terminate
    ".globl _u_fault_test",
    "_u_fault_test:",
    "  li  t0, 0x7E000000",
    "  ld  t1, 0(t0)",
    "  j   _u_fault_test",   // 不应执行到（terminate 后不恢复）
    ".globl _u_fault_test_end",
    "_u_fault_test_end:",

    // 栈写穿：sp 直接压到守护页内并写穿 → trap 时 sp 落守护页 →
    // trap_vector 栈底检查 → trap_stack_corrupt 专用路径 → terminate
    // （覆盖原 recurse 的测试点；栈顶 0xC0004000 → 0xBFFFFFF8 ∈ 守护页）
    ".globl _u_stack_overflow",
    "_u_stack_overflow:",
    "  li   t1, -16392",    // 栈顶 0xC0004000 → 0xBFFFFFF8 ∈ 守护页 [BASE-4K, BASE)
    "  add  sp, sp, t1",
    "  sd   zero, 0(sp)",    // 写守护页 → data fault；trap 时 sp 仍在守护页
    "  j    _u_stack_overflow",
    ".globl _u_stack_overflow_end",
    "_u_stack_overflow_end:",
);

unsafe extern "C" {
    static _u_ecall_test: u8;
    static _u_ecall_test_end: u8;
    static _u_fault_test: u8;
    static _u_fault_test_end: u8;
    static _u_stack_overflow: u8;
    static _u_stack_overflow_end: u8;
}

/// 用户代码页统一入口 VA（用户半区；不与 TASK_STACK_BASE 0xC0000000、
/// fault 地址 0x7E00_0000、region 地址 0x7F00_0000 冲突）。
const USER_CODE_VA: usize = 0x1_0000;

/// 取编译期汇编段的字节切片 `[start, end)`。
fn user_code(start: &'static u8, end: &'static u8) -> &'static [u8] {
    let len = end as *const u8 as usize - start as *const u8 as usize;
    unsafe { core::slice::from_raw_parts(start as *const u8, len) }
}

/// 把一段 U 代码拷入用户空间并映射 U|R|X 页，返回入口虚拟地址。
///
/// 分配一页物理帧（DRAM 恒等映射可直写），拷码后用 `space.map` 映射到用户
/// 半区 VA。demo 用途：物理帧不单独回收（随地址空间生命周期）；正式 ELF
/// loader 落地的同一路径（映射 U 代码页 + [`spawn`] 用户入口）。
fn map_user_code(space: &mut AddressSpace, code: &'static [u8], va: VirtAddr) -> VirtAddr {
    let phys = frame::allocator()
        .allocate(
            Layout::from_size_align(crate::memory::PAGE_SIZE, crate::memory::PAGE_SIZE).unwrap(),
        )
        .expect("map_user_code: frame allocation failed");
    let pa = phys.as_ptr() as *mut u8 as usize;
    // SAFETY: 物理帧刚分配（页对齐、长度 ≥ 一页），DRAM 恒等映射区可直写。
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), pa as *mut u8, code.len());
    }
    space
        .map(
            va,
            PhysAddr::from_raw(pa),
            crate::memory::PAGE_SIZE,
            PteFlags::V | PteFlags::R | PteFlags::X | PteFlags::U | PteFlags::A,
            page::allocator(),
        )
        .expect("map_user_code: map user code page failed");
    va
}

// ── 最小复现开关 ──────────────────────────────────────────
const DEMO_REGION_FAULT: bool = false; // mmap + 缺页闭环（默认开）
const DEMO_SLEEP: bool = false; // sleep 阻塞/唤醒
const DEMO_TIMER: bool = false; // 软定时器（一次性 + 周期回调）
const DEMO_RTC: bool = false; // 墙上时钟（RTC epoch 秒）
const DEMO_USER_FAULT: bool = false; // 缺页终止 + 僵尸栈回收
const DEMO_EXIT: bool = false; // 任务态显式退出
const DEMO_STACK_OVERFLOW: bool = false; // 栈写穿 → 守护页 → 专用路径 terminate（真 U 代码）
const DEMO_LEAK_CHECK: bool = true; // spawn/exit 循环 → 地址空间释放验证
const DEMO_VFS: bool = false;
const DEMO_ECALL: bool = false; // U-mode ecall → envcall 分发闭环（真 U 代码）

/// 按开关运行全部 demo（main 中调用一次）。
pub fn run() {
    if DEMO_VFS {
        demo_vfs();
    }

    // 独立地址空间 + Region，演示 mmap + 缺页闭环
    if DEMO_REGION_FAULT {
        demo_region_fault();
    }

    // sleep 阻塞 + 唤醒
    if DEMO_SLEEP {
        demo_sleep();
    }

    // 软定时器：一次性 + 周期回调（tick 驱动，中断上下文执行）
    if DEMO_TIMER {
        demo_timer();
    }

    // RTC 墙上时钟：epoch 秒 + 格式化
    if DEMO_RTC {
        demo_rtc();
    }

    // 未处理缺页 → 任务终止 + 僵尸栈回收
    if DEMO_USER_FAULT {
        demo_user_fault();
    }

    // 任务态显式退出 → 僵尸栈回收
    if DEMO_EXIT {
        demo_exit();
    }

    // 栈溢出 → 守护页缺页 → 任务终止（系统继续，而非 livelock）
    if DEMO_STACK_OVERFLOW {
        demo_stack_overflow();
    }

    // U-mode ecall → envcall 分发（scause=8）闭环 + 非法指令 terminate
    if DEMO_ECALL {
        demo_ecall();
    }

    // 泄漏回归：spawn/exit 循环 → 地址空间随 Zombie 释放（页表树归还）
    if DEMO_LEAK_CHECK {
        demo_leak_check();
    }
}

fn demo_vfs() {
    scheduler::spawn(scheduler::Entry::Kernel(vfs_test), None);
}

fn vfs_test() {
    // VFS 测试：通过文件系统接口写入 /dev/console
    {
        let fd = filesystem::open("/dev/console0", filesystem::OpenFlags::WRITE)
            .expect("vfs: open console");
        filesystem::write(fd, b"VFS: console write test\n").expect("vfs: write console");
        // 测试 /dev/null — 写入后 close
        let null_fd =
            filesystem::open("/dev/null", filesystem::OpenFlags::WRITE).expect("vfs: open null");
        filesystem::write(null_fd, b"this goes nowhere\n").expect("vfs: write null");
        filesystem::close(null_fd).expect("vfs: close null");
        filesystem::close(fd).expect("vfs: close console");
    }

    // VFS 测试：读取 /dev/log（内核日志环形缓冲）并回显到 console
    {
        let log_fd =
            filesystem::open("/dev/log", filesystem::OpenFlags::READ).expect("vfs: open log");
        let mut log_buf = [0u8; 512];
        let n = filesystem::read(log_fd, &mut log_buf).expect("vfs: read log");
        let console_fd = filesystem::open("/dev/console0", filesystem::OpenFlags::WRITE)
            .expect("vfs: open console");
        let _ = filesystem::write(console_fd, &log_buf[..n]);
        info!("[A] read {} bytes from /dev/log", n);

        // seek(Start(0)) 重读演示：VFS 偏移 API 接通（偏移由 filetable 维护，
        // File::seek 默认实现处理 Start/Current 算术）
        let off = filesystem::seek(log_fd, filesystem::SeekFrom::Start(0)).expect("vfs: seek log");
        let n2 = filesystem::read(log_fd, &mut log_buf).expect("vfs: re-read log");
        info!(
            "[B] seek to offset {off}, re-read {} bytes from /dev/log",
            n2
        );

        filesystem::close(log_fd).expect("vfs: close log");
        filesystem::close(console_fd).expect("vfs: close console");
    }

    // 演示多 console：serial 注册表数量 + /dev/console1（第二个 UART 节点）写入验证
    {
        let n = crate::uart::all().len();
        info!("[A] {} UART device(s) registered", n);
        if n > 1
            && let Ok(fd) = filesystem::open("/dev/console1", filesystem::OpenFlags::WRITE) {
                let _ = filesystem::write(fd, b"console1: secondary console write test\n");
                filesystem::close(fd).expect("vfs: close console1");
            }
    }
}

/// 演示 mmap + 缺页闭环：
/// 1. 创建独立地址空间
/// 2. 注册 Anonymous Region
/// 3. 用该空间 spawn 任务（空间随任务入队，dispatch 时由调度器激活）
/// 4. 任务访问 Region 内地址 → 缺页 → region_find 发现 → anonymous 分配零页
#[allow(dead_code)]
fn demo_region_fault() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );

    // 注册 Anonymous Region（未映射的 MMIO 间隙，不与 UART/DRAM 冲突）
    let flags = PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
    space
        .region_add(0x7F00_0000, 0x100_0000, flags, RegionKind::Anonymous)
        .expect("failed to add region");

    scheduler::spawn(scheduler::Entry::Kernel(demo_region_task), Some(space));
}

/// 演示 RTC 墙上时钟：读 epoch 秒并格式化（未注册时优雅降级）。
#[allow(dead_code)]
fn demo_rtc() {
    scheduler::spawn(scheduler::Entry::Kernel(rtc_task), None);
}

#[allow(dead_code)]
fn rtc_task() {
    match crate::hal::rtc::epoch_secs() {
        Some(s) => {
            let days = s / 86400;
            let rem = s % 86400;
            let h = rem / 3600;
            let m = (rem % 3600) / 60;
            let sec = rem % 60;
            info!("[R] epoch secs = {s} ({days}d {h:02}:{m:02}:{sec:02} since epoch)");
        }
        None => info!("[R] no RTC registered — epoch_secs() = None (优雅降级)"),
    }
}

/// 演示 sleep 阻塞 + 唤醒：阻塞 2 个定时器周期，期间其他任务被调度。
#[allow(dead_code)]
fn demo_sleep() {
    scheduler::spawn(scheduler::Entry::Kernel(sleep_task), None);
}

/// 演示软定时器：一次性(250ms) + 周期(100ms) 回调，cancel 后停止。
#[allow(dead_code)]
fn demo_timer() {
    scheduler::spawn(scheduler::Entry::Kernel(timer_task), None);
}

/// 周期回调触发计数（cancel 验证用）。
static TIMER_FIRES: AtomicUsize = AtomicUsize::new(0);

/// 一次性定时器回调（中断上下文执行）。
fn timer_once() {
    info!(
        "[T] one-shot timer fired (jiffies={})",
        crate::clock::jiffies()
    );
}

/// 周期定时器回调（中断上下文执行）。
fn timer_periodic() {
    let n = TIMER_FIRES.fetch_add(1, Ordering::Relaxed);
    info!(
        "[T] periodic timer fired #{n} (jiffies={})",
        crate::clock::jiffies()
    );
}

#[allow(dead_code)]
fn timer_task() {
    let once: crate::clock::TimerId =
        crate::clock::register(Duration::from_millis(250), &timer_once);
    let per: crate::clock::TimerId =
        crate::clock::register_periodic(Duration::from_millis(100), &timer_periodic);
    info!("[T] registered one-shot(250ms)={once:?} periodic(100ms)={per:?}");
    // 任务 sleep 1s：期间 tick 持续驱动定时器回调
    scheduler::sleep(Duration::from_secs(1));
    crate::clock::cancel(per);
    info!(
        "[T] cancelled periodic after 1s — fired {} times",
        TIMER_FIRES.load(Ordering::Relaxed)
    );
    // 取消后再等 300ms，确认不再触发
    let before = TIMER_FIRES.load(Ordering::Relaxed);
    scheduler::sleep(Duration::from_millis(300));
    let after = TIMER_FIRES.load(Ordering::Relaxed);
    info!("[T] after cancel+300ms: fires {before} → {after} (应相等)");
}

#[allow(dead_code)]
fn sleep_task() {
    // 多次 sleep 循环，压力测试重复的 park/wake 与 sepc 恢复
    for i in 0..3 {
        info!("[S] sleep task: cycle {i}, about to sleep(1s)");
        let t0 = crate::clock::now();
        scheduler::sleep(Duration::from_secs(1));
        let t1 = crate::clock::now();
        let dur = crate::clock::ticks_to_duration(t1.saturating_sub(t0));
        info!(
            "[S] sleep task: cycle {i}, woke up after {}.{:03}s",
            dur.as_secs(),
            dur.subsec_millis(),
        );
    }
    // delay 忙等演示：无任务上下文语义（自旋），与任务级 sleep 区分
    info!("[S] delay(100ms) busy-wait start");
    let t0 = crate::clock::now();
    crate::clock::delay(Duration::from_millis(100));
    let t1 = crate::clock::now();
    let dur = crate::clock::ticks_to_duration(t1.saturating_sub(t0));
    info!(
        "[S] delay done after {}.{:03}s",
        dur.as_secs(),
        dur.subsec_millis(),
    );
}

/// 演示任务态退出：显式调用 exit → 标 Zombie → tick park → 下个调度周期回收栈。
#[allow(dead_code)]
fn demo_exit() {
    scheduler::spawn(scheduler::Entry::Kernel(exit_task), None);
}

#[allow(dead_code)]
fn exit_task() {
    info!("[X] exit task: calling exit(42)");
    scheduler::exit(42);
}

/// 演示缺页终止 + 僵尸栈回收：真 U 任务执行 U 代码访问未映射地址 →
/// 未处理缺页 → terminate_current（等效 SIGSEGV）→ 栈入僵尸列表 → 下个
/// 调度周期回收。
#[allow(dead_code)]
fn demo_user_fault() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    // 无 Region：U 代码 load 0x7E00_0000 → 缺页无 Region 可解析 → terminate
    let code = unsafe { user_code(&_u_fault_test, &_u_fault_test_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    scheduler::spawn(scheduler::Entry::User(va), Some(space));
}

/// 演示栈写穿终止：真 U 任务执行 U 代码把 sp 压到守护页内并写穿 → trap 时
/// sp 落守护页 → trap_vector 栈底检查 → 专用路径 trap_stack_corrupt →
/// terminate（系统继续，无嵌套下降 livelock）。
#[allow(dead_code)]
fn demo_stack_overflow() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    // 无 Region：写守护页缺页无 Region 可解析；trap 时 sp 在守护页 → 专用路径
    let code = unsafe { user_code(&_u_stack_overflow, &_u_stack_overflow_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    scheduler::spawn(scheduler::Entry::User(va), Some(space));
}

/// 演示 U-mode ecall → envcall 分发（scause=8）完整闭环：任务真跑 U-mode，
/// 两次 ecall（未知号 → -ENOSYS 校验返回值 + sepc 跳过）→ 非法指令 terminate。
/// 日志中两次 "envcall: number=..." 即闭环证据（第二次能执行说明第一次
/// 返回值正确写回）。
#[allow(dead_code)]
fn demo_ecall() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    let code = unsafe { user_code(&_u_ecall_test, &_u_ecall_test_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    scheduler::spawn(scheduler::Entry::User(va), Some(space));
}

/// 泄漏回归：反复 spawn 立即退出的任务，验证地址空间随 Zombie 释放
/// （reclaim 打印 "reclaimed address space"，帧分配/回收平衡）。
#[allow(dead_code)]
fn demo_leak_check() {
    for _ in 0..8 {
        scheduler::spawn(scheduler::Entry::Kernel(leak_probe), None);
    }
}

#[allow(dead_code)]
fn leak_probe() {
    info!("[L] leak probe: exiting");
}

#[allow(dead_code)]
fn demo_region_task() {
    info!("task started, will trigger page fault");

    // 访问 Region 内的地址 — 首次访问触发缺页 → anonymous_region 解析
    let ptr = 0x7F00_0000 as *mut u64;
    unsafe {
        core::ptr::write_volatile(ptr, 0xDEAD);
        info!("page 0 wrote DEAD");
    }
    unsafe {
        let val = core::ptr::read_volatile(ptr);
        info!("page 0 read back {:#x}", val);
    }

    // 访问 Region 内下一页 — 再次触发缺页
    let ptr2 = 0x7F00_1000 as *mut u64;
    unsafe {
        core::ptr::write_volatile(ptr2, 0xBEEF);
        info!("page 1 wrote BEEF");
    }
    unsafe {
        let val = core::ptr::read_volatile(ptr2);
        info!("page 1 read back {:#x}", val);
    }
}
