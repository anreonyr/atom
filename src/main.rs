#![no_std]
#![no_main]
#![feature(allocator_api)]
extern crate alloc;

mod platform;
mod sbi;
mod scheduler;

#[macro_use]
mod macros;

#[macro_use]
mod print;

#[macro_use]
mod log;

mod driver;
mod filesystem;
mod hal;
mod init;
mod lock;
mod memory;
mod panic;
mod trap;

use crate::memory::allocator::page;
use crate::memory::entry::PteFlags;
use crate::memory::space::RegionKind;
use alloc::boxed::Box;
use core::arch::{asm, global_asm};

global_asm!(
    ".section .text._start",
    ".globl _early_stack_top",
    ".globl _start",
    "_start:",
    // a0 = hartid, a1 = DTB 物理地址 (RISC-V Linux boot protocol)
    // la 只修改 sp，a0/a1 原样传递给 main
    "    la   sp, _early_stack_top",
    "    j    early",
);

#[no_mangle]
/// # Safety
pub unsafe extern "C" fn early(hartid: usize, dtb_ptr: usize) -> ! {
    platform::init(dtb_ptr);

    let cfg = platform::get();
    let stack_top = cfg.dram_base + cfg.dram_size;

    asm!(
        "mv   sp, {sp}",
        "mv   a0, {hartid}",
        "jalr zero, 0({main})",
        sp = in(reg) stack_top,
        hartid = in(reg) hartid,
        main = in(reg) main,
        options(noreturn),
    );
}

#[no_mangle]
/// # Safety
pub unsafe extern "C" fn main(hartid: usize) -> ! {
    init::run().expect("kernel boot failed");

    info!(
        "hart {} booted, DRAM: {:#x}..{:#x} ({} MiB)",
        hartid,
        platform::get().dram_base,
        platform::get().dram_base + platform::get().dram_size,
        platform::get().dram_size / (1024 * 1024),
    );

    // 创建两个测试任务
    scheduler::spawn(task);

    // 第二个任务使用独立地址空间 + Region，演示 mmap + 缺页闭环
    demo_region_fault();

    info!("idle task running (wfi loop)");

    loop {
        unsafe { asm!("wfi") }
    }
}

fn task() {
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
        filesystem::close(log_fd).expect("vfs: close log");
        filesystem::close(console_fd).expect("vfs: close console");
    }

    // 演示多 console：serial 注册表数量 + /dev/console1（第二个 UART 节点）写入验证
    {
        let n = crate::driver::serial::all().len();
        info!("[A] {} UART device(s) registered", n);
        if n > 1 {
            if let Ok(fd) = filesystem::open("/dev/console1", filesystem::OpenFlags::WRITE) {
                let _ = filesystem::write(fd, b"console1: secondary console write test\n");
                filesystem::close(fd).expect("vfs: close console1");
            }
        }
    }

    let mut count = 0u64;
    loop {
        count += 1;
        println!("task count={}", count);
        for _ in 0..2_000_000 {
            unsafe { asm!("nop") }
        }
    }
}

/// 演示 mmap + 缺页闭环：
/// 1. 创建独立地址空间
/// 2. 注册 Anonymous Region
/// 3. 用该空间 spawn 任务
/// 4. 任务访问 Region 内地址 → 缺页 → region_at 发现 → anonymous_region 分配零页
fn demo_region_fault() {
    let alloc = page::allocator();
    let space = Box::leak(Box::new(
        memory::space::AddressSpace::from_kernel(alloc).expect("failed to create user space"),
    ));

    // 注册 Anonymous Region（未映射的 MMIO 间隙，不与 UART/DRAM 冲突）
    let flags = PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
    space
        .region_add(0x7F00_0000, 0x100_0000, flags, RegionKind::Anonymous)
        .expect("failed to add region");

    // 设为活动地址空间（缺页处理器通过 active_space() 找到它）
    *crate::memory::space::active_space() = Some(space);

    let root = space.root_page() as usize;
    scheduler::spawn_with(demo_region_task, root);
}

fn demo_region_task() {
    info!("[REGION] task started, will trigger page fault");

    // 访问 Region 内的地址 — 首次访问触发缺页 → anonymous_region 解析
    let ptr = 0x7F00_0000 as *mut u64;
    unsafe {
        core::ptr::write_volatile(ptr, 0xDEAD);
        info!("[REGION] page 0 wrote DEAD");
    }
    unsafe {
        let val = core::ptr::read_volatile(ptr);
        info!("[REGION] page 0 read back {:#x}", val);
    }

    // 访问 Region 内下一页 — 再次触发缺页
    let ptr2 = 0x7F00_1000 as *mut u64;
    unsafe {
        core::ptr::write_volatile(ptr2, 0xBEEF);
        info!("[REGION] page 1 wrote BEEF");
    }
    unsafe {
        let val = core::ptr::read_volatile(ptr2);
        info!("[REGION] page 1 read back {:#x}", val);
    }

    info!("[REGION] done, looping");
    loop {
        unsafe { core::arch::asm!("nop") }
    }
}
