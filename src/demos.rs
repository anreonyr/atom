// 演示任务集 — 从 main.rs 抽离的 demo 代码（评审 C7）
//
// 每个 demo 一个编译期开关（DEMO_*）+ 一个 demo_* 入口 + 若干任务函数。
// 复现某个场景时把对应开关置 true、其余保持 false——一次只跑一个 demo，
// 日志不被多个任务互相淹没。`run()` 在 main 中调用；基础任务 `task` 由 main 恒跑。

use crate::filesystem;
use crate::memory::allocator::page;
use crate::memory::entry::PteFlags;
use crate::memory::space::RegionKind;
use crate::scheduler;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

// ── 最小复现开关 ──────────────────────────────────────────
const DEMO_REGION_FAULT: bool = false; // mmap + 缺页闭环（默认开）
const DEMO_SLEEP: bool = false; // sleep 阻塞/唤醒
const DEMO_TIMER: bool = false; // 软定时器（一次性 + 周期回调）
const DEMO_RTC: bool = false; // 墙上时钟（RTC epoch 秒）
const DEMO_USER_FAULT: bool = false; // 缺页终止 + 僵尸栈回收
const DEMO_EXIT: bool = false; // 任务态显式退出
const DEMO_STACK_OVERFLOW: bool = false; // 守护页 + 栈溢出终止
const DEMO_STACK_RECURSE: bool = false; // 递归压栈溢出 → 栈底检查 → 专用路径
const DEMO_LEAK_CHECK: bool = true; // spawn/exit 循环 → 地址空间释放验证
const DEMO_VFS: bool = false;

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

    // 递归压栈溢出 → 栈底检查拦截 → 专用路径处置（User terminate）
    if DEMO_STACK_RECURSE {
        demo_stack_recurse();
    }

    // 泄漏回归：spawn/exit 循环 → 地址空间随 Zombie 释放（页表树归还）
    if DEMO_LEAK_CHECK {
        demo_leak_check();
    }
}

fn demo_vfs() {
    scheduler::spawn(vfs_test);
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
        if n > 1 {
            if let Ok(fd) = filesystem::open("/dev/console1", filesystem::OpenFlags::WRITE) {
                let _ = filesystem::write(fd, b"console1: secondary console write test\n");
                filesystem::close(fd).expect("vfs: close console1");
            }
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

    scheduler::spawn_with(demo_region_task, space);
}

/// 演示 RTC 墙上时钟：读 epoch 秒并格式化（未注册时优雅降级）。
#[allow(dead_code)]
fn demo_rtc() {
    scheduler::spawn(rtc_task);
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
    scheduler::spawn(sleep_task);
}

/// 演示软定时器：一次性(250ms) + 周期(100ms) 回调，cancel 后停止。
#[allow(dead_code)]
fn demo_timer() {
    scheduler::spawn(timer_task);
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
    scheduler::spawn(exit_task);
}

#[allow(dead_code)]
fn exit_task() {
    info!("[X] exit task: calling exit(42)");
    scheduler::exit(42);
}

/// 演示缺页终止 + 僵尸栈回收：用户空间任务访问未映射地址 → 未处理缺页
/// → terminate_current（等效 SIGSEGV）→ 栈入僵尸列表 → 下个调度周期回收。
#[allow(dead_code)]
fn demo_user_fault() {
    let alloc = page::allocator();
    let space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    // 不注册任何 Region → 任何缺页都无 Region 可解析 → 终止任务
    scheduler::spawn_with(fault_task, space);
}

#[allow(dead_code)]
fn fault_task() {
    info!("[F] fault task started, will access unmapped address");
    let ptr = 0x7E00_0000 as *mut u64;
    unsafe {
        core::ptr::write_volatile(ptr, 0xBAD);
    }
    info!("[F] this should never print (task was terminated)");
    scheduler::exit(42);
}

/// 演示守护页 + 栈溢出终止：用户任务写穿栈底 → 守护页缺页 → terminate_current
/// （系统继续运行，而非静默覆盖相邻栈 / livelock 冻结）。
#[allow(dead_code)]
fn demo_stack_overflow() {
    let alloc = page::allocator();
    let space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    // 无 Region：守护页缺页无 Region 可解析 → 终止任务
    scheduler::spawn_with(stack_overflow_task, space);
}

#[allow(dead_code)]
fn stack_overflow_task() {
    // 写指针/计数放 static：栈上的局部会被自己的写穿破坏；写用内联 asm
    // （options(nostack)，不压调用帧），sp 保持有效 → 守护页缺页时 trap
    // 帧可正常保存，干净终止（fault 地址即守护页）。
    static mut P: usize = 0;
    static mut N: usize = 0;
    info!(
        "[O] overflow task: writing past stack guard at {:#x}",
        crate::memory::TASK_STACK_BASE
    );
    unsafe {
        // 从栈底上方往下写：第一次越过 TASK_STACK_BASE（0xC0000000）
        // 落在守护页 [BASE-4K, BASE) → 缺页 → terminate_current。
        P = crate::memory::TASK_STACK_BASE + 0x1000 - 1;
        N = 0;
        while N < 64 * 1024 {
            // SAFETY: 临时诊断 demo；写穿整个栈到守护页
            core::arch::asm!(
                "sb {val}, 0({p})",
                p = in(reg) P,
                val = in(reg) 0xABu8,
                options(nostack),
            );
            P -= 1;
            N += 1;
        }
    }
    info!("[O] this should never print (task was terminated)");
    scheduler::exit(42);
}

/// 演示递归压栈溢出：无界递归写穿栈底 → trap_vector 栈底检查拦截 →
/// 专用路径 trap_stack_corrupt（User terminate，系统继续，无嵌套下降 livelock）。
#[allow(dead_code)]
fn demo_stack_recurse() {
    let alloc = page::allocator();
    let space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    scheduler::spawn_with(recurse_task, space);
}

#[allow(dead_code)]
fn recurse_task() {
    // 每帧 ~512B（局部数组），16KiB 栈约 30 层后写穿守护页 → 缺页 →
    // trap_vector 检测 sp 落在守护页 → trap_stack_corrupt → terminate。
    fn recurse(n: usize) -> usize {
        let big = [0u8; 512];
        if n == 0 {
            return 0;
        }
        core::hint::black_box(big[0]);
        recurse(n - 1) + 1
    }
    info!("[R] recurse task: starting unbounded recursion");
    let _ = recurse(100_000);
    info!("[R] this should never print (task was terminated)");
    loop {
        unsafe { core::arch::asm!("nop") }
    }
}

/// 泄漏回归：反复 spawn 立即退出的任务，验证地址空间随 Zombie 释放
/// （reclaim 打印 "reclaimed address space"，帧分配/回收平衡）。
#[allow(dead_code)]
fn demo_leak_check() {
    for _ in 0..8 {
        scheduler::spawn(leak_probe);
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
