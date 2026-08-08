// 演示任务集 — 从 main.rs 抽离的 demo 代码（评审 C7）
//
// 每个 demo 一个编译期开关（DEMO_*）+ 一个 demo_* 入口 + 若干任务函数。
// 复现某个场景时把对应开关置 true、其余保持 false——一次只跑一个 demo，
// 日志不被多个任务互相淹没。`run()` 由 shell 的 `bench` 命令触发（spawn
// 为内核任务，子任务与 shell 并行运行；默认交互仍是 shell，无需改 main）。
//
// 整个模块按开关休眠：run() 未被触发时 demo 链整体为死代码，模块级 allow
// 抑制（DEMO_* 是保留的复现开关集，非删除项）。
#![allow(dead_code)]

use crate::file;
use crate::memory::addr::{PhysAddr, VirtAddr};
use crate::memory::allocator::{frame, page};
use crate::memory::entry::PteFlags;
use crate::memory::space::{AddressSpace, RegionKind};
use crate::schedule::{self, Entry, TaskBuilder};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::alloc::Layout;
use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

// ── U-mode 测试代码段 ──────────────────────────────────────────
//
// 编译期汇编（global_asm），链接进内核镜像 .text（link.ld 的 *(.text*)
// 捕获 .text.user_code 子段）；运行时由 [`map_user_code`] 拷入用户页执行。
// 约束：仅用寄存器 + 立即数 + 相对分支/相对寻址（位置无关 PIC），不访问
// 任何全局地址——拷到用户空间后独立可运行。ecall 闭环校验：未知号 ×2
// → -ENOSYS；write(1, ...) → stdout 输出；exit(0) → 干净退出（不再走
// 非法指令 terminate——退出码经 wait 收尸可见）。
global_asm!(
    ".section .text.user_code, \"ax\"",
    // ecall 闭环：未知号 ×2（期望 -ENOSYS）→ write 输出 → exit 干净退出
    ".globl _u_ecall_test",
    "_u_ecall_test:",
    "  li  a7, 0xED", // 未知调用号
    "  li  a0, 0x1234",
    "  ecall",          // → trap_handler scause=8 → envcall::dispatch
    "  li  t0, -38",    // 期望返回值 = -ENOSYS（两补）
    "  bne a0, t0, 2f", // 校验失败也走非法指令 terminate（日志可辨）
    "  li  a7, 0xEE",   // 第二次 ecall：验证 sepc+4 跳过、可重复
    "  li  a0, 0x5678",
    "  ecall",
    "  bne a0, t0, 2f",
    // write(1, "hello from user\n", 16) → preferred 输出设备
    "  li    a0, 1",  // fd = stdout
    "  la    a1, 1f", // 字符串地址（PC 相对，拷到用户空间不变）
    "  li    a2, 16", // 字节数（"hello from user\n" = 16）
    "  li    a7, 64", // write
    "  ecall",
    "  bne   a0, a2, 2f", // 校验写满 16 字节
    // exit(0) → 标 Zombie（退出码 0）后 trap 态直接切下一任务
    "  li    a0, 0",
    "  li    a7, 93", // exit
    "  ecall",        // 不返回（DispatchResult::Terminate）
    "  j     2f",     // 防御：exit 若返回则落非法指令（日志可辨）
    "1:",
    "  .ascii \"hello from user\\n\"",
    "2:",
    "  .word 0",
    ".globl _u_ecall_test_end",
    "_u_ecall_test_end:",
    // 未映射访问：load 0x7E00_0000 → 缺页（scause=13）→ terminate
    ".globl _u_fault_test",
    "_u_fault_test:",
    "  li  t0, 0x7E000000",
    "  ld  t1, 0(t0)",
    "  j   _u_fault_test", // 不应执行到（terminate 后不恢复）
    ".globl _u_fault_test_end",
    "_u_fault_test_end:",
    // 栈写穿：sp 直接压到守护页内并写穿 → trap 时 sp 落守护页 →
    // trap_vector 栈底检查 → trap_stack_corrupt 专用路径 → terminate
    // （覆盖原 recurse 的测试点；栈顶 0xC0004000 → 0xBFFFFFF8 ∈ 守护页）
    ".globl _u_stack_overflow",
    "_u_stack_overflow:",
    "  li   t1, -16392", // 栈顶 0xC0004000 → 0xBFFFFFF8 ∈ 守护页 [BASE-4K, BASE)
    "  add  sp, sp, t1",
    "  sd   zero, 0(sp)", // 写守护页 → data fault；trap 时 sp 仍在守护页
    "  j    _u_stack_overflow",
    ".globl _u_stack_overflow_end",
    "_u_stack_overflow_end:",
    // 输入闭环：ecall read(0, sp, 1) 阻塞读 stdin → 校验返回 1 且字节非 0
    // → 非法指令 terminate。阻塞期间任务被 tick 切走，字符到达后唤醒重放
    // ecall（dispatch 重入，缓冲已非空直接读）——日志中的 debug! 即证据。
    ".globl _u_read_test",
    "_u_read_test:",
    "  addi  sp, sp, -16", // 栈上留输入缓冲（用户栈已映射 U|R|W）
    "  mv    a1, sp",      // buf = sp
    "  li    a2, 1",       // count = 1
    "  li    a0, 0",       // fd = stdin
    "  li    a7, 63",      // READ
    "  ecall",
    "  li    t0, 1", // 期望返回 1（阻塞读到 1 字节）
    "  bne   a0, t0, 2f",
    "  lbu   t0, 0(sp)",    // 读回字节
    "  beq   t0, zero, 2f", // 非 0 才算真读到
    "  addi  sp, sp, 16",
    "  .word 0", // 完成 → 非法指令 terminate（demo 结束标志）
    "2:",
    "  .word 0",
    ".globl _u_read_test_end",
    "_u_read_test_end:",
    // 用户堆闭环：map(4096) → 写魔数 → 读回校验 → unmap → exit(0)
    ".globl _u_map_test",
    "_u_map_test:",
    "  li    a0, 4096", // map(size=4096)
    "  li    a7, 1000", // MAP（自定义号段；堆匿名分配）
    "  ecall",
    "  li    t0, 0x20000000", // 期望：堆区基址（≥ USER_HEAP_BASE）
    "  blt   a0, t0, 2f",     // 返回 < 堆基址 → 错误路径
    "  mv    s0, a0",         // 保存 VA（s0 callee-saved，trap 保存/恢复）
    "  li    t1, 0xDEADBEEF",
    "  sd    t1, 0(s0)",  // 写魔数
    "  ld    t2, 0(s0)",  // 读回
    "  bne   t1, t2, 2f", // 校验写读一致
    "  mv    a0, s0",     // unmap(addr, size)
    "  li    a1, 4096",
    "  li    a7, 1001", // UNMAP（自定义号段）
    "  ecall",
    "  bnez  a0, 2f", // unmap 应返回 0
    "  li    a0, 0",  // exit(0) → 干净退出
    "  li    a7, 93",
    "  ecall", // 不返回
    "2:",
    "  .word 0",
    ".globl _u_map_test_end",
    "_u_map_test_end:",
    // filesystem ecall 闭环：open("/dev/stdout", O_WRONLY) → write → close → exit
    // （日志中 envcall: open/close 的 info 输出 + 控制台 "u-open!" 即闭环证据）
    ".globl _u_open_test",
    "_u_open_test:",
    "  la    a0, 3f",   // path = "/dev/stdout"（PC 相对，拷到用户空间不变）
    "  li    a1, 1",    // flags = O_WRONLY（accmode 低 2 位）
    "  li    a2, 0",    // mode = 0（无创建语义，忽略）
    "  li    a7, 1002", // OPEN（自定义号段）
    "  ecall",
    "  bltz  a0, 2f", // fd < 0（-errno）→ 错误路径
    "  mv    s0, a0", // 保存 fd（callee-saved，trap 保存/恢复）
    "  mv    a0, s0", // write(fd, msg, len)
    "  la    a1, 4f",
    "  li    a2, 8",  // "u-open!\n" = 8 字节
    "  li    a7, 64", // WRITE
    "  ecall",
    "  bne   a0, a2, 2f", // 校验写满
    "  mv    a0, s0",     // close(fd)
    "  li    a7, 1003",   // CLOSE（自定义号段）
    "  ecall",
    "  bnez  a0, 2f", // close 应返回 0
    // 负路径：open("/dev/stdout", O_RDONLY=0) 后 write → 期望 -EACCES（-13）
    "  la    a0, 3f",   // path 复用（同 "/dev/stdout"）
    "  li    a1, 0",    // flags = O_RDONLY（accmode 低 2 位）
    "  li    a2, 0",    // mode = 0（忽略）
    "  li    a7, 1002", // OPEN
    "  ecall",
    "  bltz  a0, 2f", // fd < 0 → 错误路径
    "  mv    s0, a0", // 保存只读 fd
    "  mv    a0, s0", // write(只读 fd, msg, len) → 期望 -EACCES
    "  la    a1, 4f",
    "  li    a2, 8",
    "  li    a7, 64", // WRITE
    "  ecall",
    "  li    t0, -13",    // 期望 -EACCES（两补）
    "  bne   a0, t0, 2f", // 非 -13 → 错误路径
    "  mv    a0, s0",     // close(只读 fd)
    "  li    a7, 1003",
    "  ecall",
    "  bnez  a0, 2f", // close 应返回 0
    "  li    a0, 0",  // exit(0) → 干净退出
    "  li    a7, 93",
    "  ecall",    // 不返回
    "  j     2f", // 防御：exit 若返回则落非法指令（日志可辨）
    "3:",
    "  .asciz \"/dev/stdout\"",
    "4:",
    "  .ascii \"u-open!\\n\"",
    "2:",
    "  .word 0",
    ".globl _u_open_test_end",
    "_u_open_test_end:",
);

unsafe extern "C" {
    static _u_ecall_test: u8;
    static _u_ecall_test_end: u8;
    static _u_fault_test: u8;
    static _u_fault_test_end: u8;
    static _u_stack_overflow: u8;
    static _u_stack_overflow_end: u8;
    static _u_read_test: u8;
    static _u_read_test_end: u8;
    static _u_map_test: u8;
    static _u_map_test_end: u8;
    static _u_open_test: u8;
    static _u_open_test_end: u8;
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
const DEMO_EXIT: bool = true; // 任务态显式退出
const DEMO_STACK_OVERFLOW: bool = true; // 栈写穿 → 守护页 → 专用路径 terminate（真 U 代码）
const DEMO_LEAK_CHECK: bool = false; // spawn/exit 循环 → 地址空间释放验证
const DEMO_VFS: bool = false;
const DEMO_ECALL: bool = true; // U-mode ecall → envcall 分发闭环（真 U 代码）
const DEMO_MAP: bool = true; // 用户堆：map/unmap 匿名页分配闭环（真 U 代码）
const DEMO_OPEN: bool = true; // filesystem ecall：open/write/close 闭环（真 U 代码）
const DEMO_BLOCK: bool = true; // 块设备 DMA 冒烟：读高数据块 → 写魔数 → 读回比对
const DEMO_FS: bool = true; // 极简 FS 冒烟：create /data 文件 → 写 → 读回比对 + readdir
const DEMO_LOADER: bool = true; // ELF loader：装载内嵌用户程序 ELF → spawn → wait 收退出码
const DEMO_WAIT: bool = true; // wait 收尸 + 事件阻塞 + 退出码
const DEMO_KILL: bool = true; // kill 他杀 + 唤醒等待者 + 错误路径
const DEMO_YIELD: bool = true; // r#yield 主动让出（self-IPI 立即重排）
const DEMO_PRIORITY: bool = true; // 加权时间片：priority → 时间片长度（TaskBuilder）
const DEMO_THREAD: bool = true; // 线程语义：共享地址空间 + Arc 引用计数
const DEMO_INPUT: bool = false; // 内核任务阻塞读 console 输入（需交互：敲键盘）
const DEMO_INPUT_USER: bool = false; // U-mode ecall read 阻塞读 stdin（需交互：敲键盘）

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

    // 用户堆：map/unmap 匿名页分配 → 写读 → 释放闭环
    if DEMO_MAP {
        demo_map();
    }

    // filesystem ecall：open → write → close → exit 闭环
    if DEMO_OPEN {
        demo_open();
    }

    // 块设备 DMA 冒烟：读高数据块 → 写魔数 → 读回比对（M4 virtio-blk 驱动验证）
    if DEMO_BLOCK {
        demo_block();
    }

    // 极简 FS 冒烟：create /data 文件 → 写 → 读回比对 + readdir（M4 file::fs 验证）
    if DEMO_FS {
        demo_fs();
    }

    // ELF loader：装载内嵌用户程序 ELF → spawn → wait 收回退出码（M1 验收）
    if DEMO_LOADER {
        demo_loader();
    }

    // wait 父子回收：收尸 + 事件阻塞 + 退出码
    if DEMO_WAIT {
        demo_wait();
    }

    // kill 他杀：终止目标任务 + 唤醒等待者 + 错误路径
    if DEMO_KILL {
        demo_kill();
    }

    // r#yield 主动让出：self-IPI 立即重排
    if DEMO_YIELD {
        demo_yield();
    }

    // 加权时间片：高优先级任务（0 → 16 tick）与默认任务（128 → 8 tick）
    // 同起跑固定自增，对比完成耗时（时间片 2:1）
    if DEMO_PRIORITY {
        demo_priority();
    }

    // 线程语义：两线程共享同一地址空间（Arc），共享页写读验证
    if DEMO_THREAD {
        demo_thread();
    }

    // 输入：内核任务阻塞读 console（敲键盘后读到并回显日志）
    if DEMO_INPUT {
        demo_input();
    }

    // 输入：U-mode ecall read（敲键盘后 U 任务读到并校验退出）
    if DEMO_INPUT_USER {
        demo_input_user();
    }

    // 泄漏回归：spawn/exit 循环 → 地址空间随 Zombie 释放（页表树归还）
    if DEMO_LEAK_CHECK {
        demo_leak_check();
    }
}

fn demo_vfs() {
    TaskBuilder::new(Entry::Kernel(vfs_test)).spawn();
}

fn vfs_test() {
    // VFS 测试：通过文件系统接口写入 /dev/console
    {
        let fd = file::open("/dev/console0", file::OpenFlags::WRITE).expect("vfs: open console");
        file::write(fd, b"VFS: console write test\n").expect("vfs: write console");
        // 测试 /dev/null — 写入后 close
        let null_fd = file::open("/dev/null", file::OpenFlags::WRITE).expect("vfs: open null");
        file::write(null_fd, b"this goes nowhere\n").expect("vfs: write null");
        file::close(null_fd).expect("vfs: close null");
        file::close(fd).expect("vfs: close console");
    }

    // 演示 /dev/console 符号链接（preferred 表达）：open 经 lookup 跟随链接
    // → consoleN Inode → 终端 File（写 CRLF）
    {
        let fd =
            file::open("/dev/console", file::OpenFlags::WRITE).expect("vfs: open console symlink");
        file::write(fd, b"VFS: console symlink write\n").expect("vfs: write via symlink");
        file::close(fd).expect("vfs: close console symlink");
    }
    // 演示 /dev/uartN 原始字节流（无终端语义：write 无 CRLF 转换）
    {
        let fd = file::open("/dev/uart0", file::OpenFlags::WRITE).expect("vfs: open uart0 raw");
        file::write(fd, b"VFS: uart0 raw write (no CRLF)\n").expect("vfs: write uart0 raw");
        file::close(fd).expect("vfs: close uart0 raw");
    }
    // 演示多 console：serial 注册表数量 + /dev/console1（第二个 UART 节点）写入验证
    {
        let n = crate::io::console::count();
        info!("[A] {} UART device(s) registered", n);
        if n > 1
            && let Ok(fd) = file::open("/dev/console1", file::OpenFlags::WRITE)
        {
            let _ = file::write(fd, b"console1: secondary console write test\n");
            file::close(fd).expect("vfs: close console1");
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
    let space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );

    // 注册 Anonymous Region（未映射的 MMIO 间隙，不与 UART/DRAM 冲突）
    let flags = PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
    space
        .declare(0x7F00_0000, 0x100_0000, flags, RegionKind::Anonymous)
        .expect("failed to add region");

    TaskBuilder::new(Entry::Kernel(demo_region_task))
        .space(Some(space))
        .spawn();
}

/// 演示 RTC 墙上时钟：读 epoch 秒并格式化（未注册时优雅降级）。
#[allow(dead_code)]
fn demo_rtc() {
    TaskBuilder::new(Entry::Kernel(rtc_task)).spawn();
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
    TaskBuilder::new(Entry::Kernel(sleep_task)).spawn();
}

/// 演示软定时器：一次性(250ms) + 周期(100ms) 回调，cancel 后停止。
#[allow(dead_code)]
fn demo_timer() {
    TaskBuilder::new(Entry::Kernel(timer_task)).spawn();
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
    schedule::sleep(Duration::from_secs(1));
    crate::clock::cancel(per);
    info!(
        "[T] cancelled periodic after 1s — fired {} times",
        TIMER_FIRES.load(Ordering::Relaxed)
    );
    // 取消后再等 300ms，确认不再触发
    let before = TIMER_FIRES.load(Ordering::Relaxed);
    schedule::sleep(Duration::from_millis(300));
    let after = TIMER_FIRES.load(Ordering::Relaxed);
    info!("[T] after cancel+300ms: fires {before} → {after} (应相等)");
}

#[allow(dead_code)]
fn sleep_task() {
    // 多次 sleep 循环，压力测试重复的 park/wake 与 sepc 恢复
    for i in 0..3 {
        info!("[S] sleep task: cycle {i}, about to sleep(1s)");
        let t0 = crate::clock::now();
        schedule::sleep(Duration::from_secs(1));
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
    TaskBuilder::new(Entry::Kernel(exit_task)).spawn();
}

#[allow(dead_code)]
fn exit_task() {
    info!("[X] exit task: calling exit(42)");
    schedule::exit(42);
}

/// 演示 wait/waitpid：父任务等子退出取退出码。
/// 场景 A：父先等、子后退出 → Reap 处置唤醒父并当场收尸（无僵尸残留）；
/// 场景 B：子先退出（僵尸因 parent alive 被保留）、父后 wait → 直接收尸；
/// 场景 C：wait 不存在的任务 → None（不阻塞）。
#[allow(dead_code)]
fn demo_wait() {
    TaskBuilder::new(Entry::Kernel(wait_parent)).spawn();
}

#[allow(dead_code)]
fn wait_parent() {
    // A：父先等、子后退出 → Reap 处置唤醒父 + 当场收尸
    let child = TaskBuilder::new(Entry::Kernel(wait_child)).spawn();
    let code = schedule::wait(child);
    info!("[W] wait(child {child:#x}) = {code:?} (期望 Some(42))");
    // B：子先退、父后 wait（子 zombie 因 parent alive 被保留）
    let child2 = TaskBuilder::new(Entry::Kernel(wait_child_2)).spawn();
    schedule::sleep(Duration::from_millis(50)); // 让子跑完 exit(7) 并入僵尸
    let code2 = schedule::wait(child2);
    info!("[W] wait(child2 {child2:#x}) = {code2:?} (期望 Some(7))");
    // C：wait 不存在的任务 → None（不阻塞）
    let code3 = schedule::wait(0xDEAD);
    info!("[W] wait(nonexistent) = {code3:?} (期望 None)");
}

#[allow(dead_code)]
fn wait_child() {
    schedule::exit(42);
}

#[allow(dead_code)]
fn wait_child_2() {
    schedule::exit(7);
}

/// 演示 kill（他杀）：终止就绪/睡眠任务、kill 自身/不存在的错误路径、
/// 以及「父 wait 阻塞中目标被 kill → wait 返回 None」（Killed 路径）。
static KILL_ID: AtomicUsize = AtomicUsize::new(0);

#[allow(dead_code)]
fn demo_kill() {
    TaskBuilder::new(Entry::Kernel(kill_parent)).spawn();
}

#[allow(dead_code)]
fn kill_parent() {
    // A：kill 睡眠中的子任务
    let child = TaskBuilder::new(Entry::Kernel(kill_target_task)).spawn();
    schedule::sleep(Duration::from_millis(30)); // 子进入睡眠循环
    let r = schedule::kill(child);
    info!("[K] kill(sleeping {child:#x}) = {r:?} (期望 Ok(()))");
    // B：kill 不存在的任务
    let r2: Result<(), schedule::KillError> = schedule::kill(0xCAFE);
    info!("[K] kill(nonexistent) = {r2:?} (期望 Err(NotFound))");
    // C：kill 自己 → IsCurrent
    let me = crate::schedule::current_id();
    let r3 = schedule::kill(me);
    info!("[K] kill(self {me:#x}) = {r3:?} (期望 Err(IsCurrent))");
    // D：wait 一个已被 kill 的目标 → None（目标已死，无僵尸可收）
    let child4 = TaskBuilder::new(Entry::Kernel(kill_target_task)).spawn();
    schedule::sleep(Duration::from_millis(30));
    let _ = schedule::kill(child4);
    let r4 = schedule::wait(child4);
    info!("[K] wait(killed {child4:#x}) = {r4:?} (期望 None)");
    // E：父 wait 阻塞中，killer 任务 kill 目标 → wait 返回 None（Killed 路径）
    let child5 = TaskBuilder::new(Entry::Kernel(kill_target_task)).spawn();

    KILL_ID.store(child5, Ordering::Relaxed);
    TaskBuilder::new(Entry::Kernel(killer_task)).spawn();

    let r5 = schedule::wait(child5);
    info!("[K] wait(killed-while-waiting {child5:#x}) = {r5:?} (期望 None)");
    // F：父 wait 与 killer 立即 kill 竞争（无 sleep）。这是不死锁冒烟测试：
    // 修复前② alive 与③ 置 Wait 的窗口只有两条指令宽，demo 大概率打不中；
    // 真正的保证来自 wait() 的①收尸/②判活/③置Wait 同一关中断区间。两种
    // 时序（kill 先 / 后）结果都是 None，验证不死锁即可。
    let child6 = TaskBuilder::new(Entry::Kernel(kill_target_task)).spawn();
    KILL_ID.store(child6, Ordering::Relaxed);
    TaskBuilder::new(Entry::Kernel(killer_task_immediate)).spawn();
    let r6 = schedule::wait(child6);
    info!("[K] wait(race-killed {child6:#x}) = {r6:?} (期望 None，不死锁)");
}

#[allow(dead_code)]
fn kill_target_task() {
    loop {
        schedule::sleep(Duration::from_millis(100));
    }
}

#[allow(dead_code)]
fn killer_task() {
    let id = KILL_ID.load(Ordering::Relaxed);
    schedule::sleep(Duration::from_millis(20)); // 等父进入 wait
    let r = schedule::kill(id);
    info!("[K] killer killed {id:#x} = {r:?}");
}

#[allow(dead_code)]
fn killer_task_immediate() {
    let id = KILL_ID.load(Ordering::Relaxed);
    let r = schedule::kill(id); // 不 sleep：与父 wait 竞争
    info!("[K] killer(immediate) killed {id:#x} = {r:?}");
}

/// 演示 r#yield 主动让出：两个任务交替 yield——A 让出后 B 的 before 应
/// 紧接着出现（立即重排），而非等 10ms tick。
#[allow(dead_code)]
fn demo_yield() {
    TaskBuilder::new(Entry::Kernel(yield_task_a)).spawn();
    TaskBuilder::new(Entry::Kernel(yield_task_b)).spawn();
}

#[allow(dead_code)]
fn yield_task_a() {
    for i in 0..5 {
        info!("[Y] A before yield #{i}");
        schedule::r#yield();
        info!("[Y] A after yield #{i}");
    }
    info!("[Y] A done");
}

#[allow(dead_code)]
fn yield_task_b() {
    for i in 0..5 {
        info!("[Y] B before yield #{i}");
        schedule::r#yield();
        info!("[Y] B after yield #{i}");
    }
    info!("[Y] B done");
}

/// 演示加权时间片：高优先级任务（priority=0 → 16 tick）与默认任务
/// （priority=128 → 8 tick）同起跑、相同固定自增量，对比完成耗时——
/// 时间片 2:1 → CPU 份额 2:1，高优先级任务应明显更快完成（实测 QEMU：
/// 20M 次自增 2.7s vs 3.4s，符合两任务系统理论值 1.33:1）。用 TaskBuilder
/// 指定优先级（spawn 便捷入口保持默认）。
#[allow(dead_code)]
fn demo_priority() {
    TaskBuilder::new(Entry::Kernel(prio_task_high))
        .priority(0)
        .spawn();
    TaskBuilder::new(Entry::Kernel(prio_task_default)).spawn();
}

/// 高优先级对比任务：priority=0 → 16 tick（160ms）。
#[allow(dead_code)]
fn prio_task_high() {
    prio_run(0);
}

/// 默认优先级对比任务：priority=128 → 8 tick（80ms）。
#[allow(dead_code)]
fn prio_task_default() {
    prio_run(128);
}

/// 固定量自增（纯 CPU，便于对比时间片占比），打印耗时后退出。
fn prio_run(prio: u8) {
    let t0 = crate::clock::now();
    let mut n: u32 = 0;
    for _ in 0..20_000_000 {
        n = n.wrapping_add(1);
    }
    let t1 = crate::clock::now();
    let dur = crate::clock::ticks_to_duration(t1.saturating_sub(t0));
    info!(
        "[P] prio={prio}: {n} increments in {}.{:03}s",
        dur.as_secs(),
        dur.subsec_millis(),
    );
    schedule::exit(0);
}

/// 演示线程语义（共享地址空间）——boot 上下文只做 spawn（sie 使能前不可
/// sleep/wfi），所有等待与验证移入组长任务（[`thread_leader`]）：
/// 1. 创建空间并映射一块共享用户区页（仅本空间可见——独立任务克隆空间
///    看不到该映射，这是线程与任务的核心差异）
/// 2. 组长 + 两个线程全部经 [`schedule::TaskBuilder::shared`] 共享同一空间
///    （Arc 引用计数：三者各持一份，最后退出才释放页表树）
/// 3. 线程写共享页不同偏移（共享内存即通信），组长 sleep 后读回验证
#[allow(dead_code)]
fn demo_thread() {
    let alloc = page::allocator();
    let space = Box::new(AddressSpace::from_kernel(alloc).expect("create thread space"));

    // 共享物理页：申请一帧映射到用户区 VA（仅本空间可见；共享页表即共享内存）
    let layout =
        Layout::from_size_align(crate::memory::PAGE_SIZE, crate::memory::PAGE_SIZE).unwrap();
    let shared = frame::allocator().allocate(layout).expect("shared page");
    let shared_pa = shared.as_ptr() as *mut u8 as usize;
    let flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::U | PteFlags::A | PteFlags::D;
    space
        .map(
            VirtAddr::from_raw(SHARED_VA),
            PhysAddr::from_raw(shared_pa),
            crate::memory::PAGE_SIZE,
            flags,
            alloc,
        )
        .expect("map shared page");

    let space = Arc::new(*space);
    // 线程 A/B + 组长任务：三者共享同一空间（各自独立栈窗口，动态错开）
    TaskBuilder::new(Entry::Kernel(thread_writer_a))
        .shared(Arc::clone(&space))
        .spawn();
    TaskBuilder::new(Entry::Kernel(thread_writer_b))
        .shared(Arc::clone(&space))
        .spawn();
    TaskBuilder::new(Entry::Kernel(thread_leader))
        .shared(space)
        .spawn();
}

/// 组长任务：等线程写完（各 3 次 × 30ms）后读共享页验证，最后退出
/// （Arc 计数归零 → 空间页表树释放）。
#[allow(dead_code)]
fn thread_leader() {
    schedule::sleep(Duration::from_millis(300));
    let ptr = SHARED_VA as *const u32;
    // SAFETY: 共享页已映射且线程已写；SUM=1（trap 入口置位）使内核任务可访问用户页
    let a = unsafe { *ptr };
    let b = unsafe { *ptr.add(1) };
    info!("[T] leader reads shared page: a={a} b={b}");
    schedule::exit(0);
}

/// 线程 A：写共享页 [0] 递增。
#[allow(dead_code)]
fn thread_writer_a() {
    let ptr = SHARED_VA as *mut u32;
    for i in 0..3 {
        // SAFETY: 共享页已映射（SUM=1）
        unsafe {
            *ptr = i;
        }
        schedule::sleep(Duration::from_millis(30));
    }
    info!("[T] thread_a done (last write {})", unsafe { *ptr });
    schedule::exit(0);
}

/// 线程 B：写共享页 [1]（*10 便于区分）。
#[allow(dead_code)]
fn thread_writer_b() {
    let ptr = (SHARED_VA + 4) as *mut u32;
    for i in 0..3 {
        // SAFETY: 共享页已映射（SUM=1）
        unsafe {
            *ptr = i * 10;
        }
        schedule::sleep(Duration::from_millis(30));
    }
    info!("[T] thread_b done (last write {})", unsafe { *ptr });
    schedule::exit(0);
}

/// 共享用户区页 VA（与 demo_region_fault 同区，不与 DRAM/MMIO 冲突）。
const SHARED_VA: usize = 0x7F00_0000;

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
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
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
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
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
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
}

/// 演示用户堆 map/unmap 闭环：U 任务 map(4096) 得堆区 VA → 写魔数 →
/// 读回校验 → unmap 释放 → exit(0)。复用内核 frame 分配器供页。
#[allow(dead_code)]
fn demo_map() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    let code = unsafe { user_code(&_u_map_test, &_u_map_test_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
}

/// 演示 filesystem ecall 闭环：U 任务 open("/dev/stdout", O_WRONLY) →
/// write 输出 "u-open!" → close → exit(0)。日志中 envcall: open/close 的
/// info 输出 + 控制台 "u-open!" 即闭环证据（fd 经 VFS 全局表解析）。
#[allow(dead_code)]
fn demo_open() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    let code = unsafe { user_code(&_u_open_test, &_u_open_test_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
}

/// 演示块设备 DMA 冒烟（M4 virtio-blk）：读盘尾块 → 写魔数 → 读回比对。
///
/// 选盘尾数据块（不碰 FS 元数据/测试文件区，见 file/fs 布局）；日志
/// `[B] block N readback magic 0xdeadbeef ... OK` 即 DMA 读/写闭环证据。
#[allow(dead_code)]
fn demo_block() {
    let Some(dev) = crate::hal::block::get() else {
        info!("[B] no block device registered");
        return;
    };
    let bs = dev.block_size();
    let last = dev.block_count().saturating_sub(1) as u32; // 盘尾块
    let mut buf = alloc::vec![0u8; bs];
    dev.read_block(last, &mut buf);
    let magic = 0xDEADBEEFu32;
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    dev.write_block(last, &buf);
    let mut back = alloc::vec![0u8; bs];
    dev.read_block(last, &mut back);
    let v = u32::from_le_bytes([back[0], back[1], back[2], back[3]]);
    info!(
        "[B] block {last} readback magic {v:#x} (expect {magic:#x}) {}",
        if v == magic { "OK" } else { "MISMATCH" }
    );
}

/// 演示极简 FS（M4 file::fs 冒烟）：create `/data/message` → 写 → 读回比对 +
/// readdir 列举。首次 boot create（文件不存在），后续 boot open 已有文件重写。
/// 无块设备（`/data` 未挂载）→ 降级跳过（日志提示，不 panic——`bench` 在无盘
/// 环境也应可跑，仅跳过 FS 相关演示）。
/// 日志 `[FS] /data/message readback N bytes ... OK` + `[FS] readdir /data` 即
/// FS 建/写/读/列闭环证据。
#[allow(dead_code)]
fn demo_fs() {
    // 无块设备 → /data 未挂载，跳过（FS 冒烟需磁盘）
    if crate::hal::block::get().is_none() {
        info!("[FS] no block device — /data not mounted, skipping");
        return;
    }
    // open 不存在 → ENOENT；create（O_CREAT 语义，已存在则返回现有）。
    // /data 已挂载前提下 create 不应失败；失败是真实 FS 缺陷（expect 暴露）。
    let fd = match file::open("/data/message", file::OpenFlags::WRITE) {
        Ok(fd) => fd,
        Err(_) => file::create("/data/message", file::OpenFlags::WRITE).expect("fs: create"),
    };
    let content = b"hello fs\n";
    file::write(fd, content).expect("fs: write");
    file::close(fd).expect("fs: close");

    // 读回比对
    let fd = file::open("/data/message", file::OpenFlags::READ).expect("fs: reopen");
    let mut buf = [0u8; 64];
    let n = file::read(fd, &mut buf).expect("fs: read");
    file::close(fd).expect("fs: close");
    let ok = n == content.len() && &buf[..n] == content;
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("<bad utf8>");
    info!(
        "[FS] /data/message readback {} bytes: {text:?} {}",
        n,
        if ok { "OK" } else { "MISMATCH" }
    );

    // readdir 列举 /data
    if let Ok(dfd) = file::open("/data", file::OpenFlags::READ) {
        let mut names = [0u8; 256];
        if let Ok(m) = file::readdir(dfd, &mut names) {
            let text = core::str::from_utf8(&names[..m]).unwrap_or("<bad utf8>");
            info!("[FS] readdir /data: {text:?}");
        }
        let _ = file::close(dfd);
    }
}

/// 演示 ELF loader（M1 验收）：装载内嵌的用户程序 ELF（真实 ELF64，非内嵌
/// 汇编）→ spawn 真 U-mode 任务 → wait 收回退出码。控制台输出 "hello from
/// elf" + 日志 `envcall: write`/`exit` + `[ELF] wait(...) = Some(0)` 即闭环
/// 证据——区别于 map_user_code 的手工拷贝，这里是 loader 按 PT_LOAD 段装载。
#[allow(dead_code)]
fn demo_loader() {
    let blob = crate::loader::user_program();
    match TaskBuilder::loader(blob) {
        Ok(builder) => {
            let id = builder.spawn();
            let code = schedule::wait(id);
            info!("[ELF] wait({id:#x}) = {code:?} (期望 Some(0))");
        }
        Err(e) => info!("[ELF] loader error: {e:?}"),
    }
}

/// 泄漏回归：反复 spawn 立即退出的任务，验证地址空间随 Zombie 释放
/// （reclaim 打印 "reclaimed address space"，帧分配/回收平衡）。
#[allow(dead_code)]
fn demo_leak_check() {
    for _ in 0..8 {
        TaskBuilder::new(Entry::Kernel(leak_probe)).spawn();
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

/// 演示内核任务阻塞读 console 输入：`crate::io::stdin().read` 阻塞等字符
/// （缓冲空 → schedule::input_wait 任务 park），敲键盘后读到并回显日志。
#[allow(dead_code)]
fn demo_input() {
    TaskBuilder::new(Entry::Kernel(input_task)).spawn();
}

#[allow(dead_code)]
fn input_task() {
    let mut buf = [0u8; 8];
    match crate::io::stdin().read(&mut buf) {
        Ok(n) => info!("[I] kernel read {} bytes: {:?}", n, &buf[..n]),
        Err(e) => info!("[I] kernel read failed: {:?}", e),
    }
    schedule::exit(0);
}

/// 演示 U-mode ecall read 闭环：真 U 任务阻塞读 stdin（`_u_read_test`），
/// 读到 1 字节且非 0 后校验通过 → 非法指令 terminate（日志中的 envcall
/// read debug 输出即读到证据；重放路径：tick 切走 → 字符到达唤醒 →
/// sret 到 ecall 重入分发）。
#[allow(dead_code)]
fn demo_input_user() {
    let alloc = page::allocator();
    let mut space = Box::new(
        crate::memory::space::AddressSpace::from_kernel(alloc)
            .expect("failed to create user space"),
    );
    let code = unsafe { user_code(&_u_read_test, &_u_read_test_end) };
    let va = map_user_code(&mut space, code, VirtAddr::from_raw(USER_CODE_VA));
    TaskBuilder::new(Entry::User(va)).space(Some(space)).spawn();
}
