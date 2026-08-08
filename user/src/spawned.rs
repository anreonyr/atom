// 子程序验收探针 — M5 spawn 的子任务（独立 ELF，父程序 spawn 它）
//
// 无 libc、无运行时：`_start` 直接发 syscall（ecall 陷入内核）。被 user 程序
// 经 `spawn(blob)` syscall 装载进**新地址空间**运行（loader 逐段映射）——区别于
// 线程共享空间，本程序是独立进程。流程：write 输出 "child program speaking" →
// exit(42)，父 `wait` 收回 42 即 M5 验收闭环。
//
// 由 scripts/build-user.sh 先于 user 构建（user 的 include_bytes! 需本产物）。
#![no_std]
#![no_main]

use core::arch::asm;

// ── syscall 号（与内核 runtime/envcall.rs 常量一致） ──────
const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
extern "C" fn _start() -> ! {
    // write(1, "[M5] child program speaking\n") — 独立输出（fd 经全局 filetable）
    let msg: &[u8] = b"[M5] child program speaking\n";
    unsafe {
        asm!(
            "ecall",
            in("a7") SYS_WRITE,
            in("a0") 1usize,
            in("a1") msg.as_ptr() as usize,
            in("a2") msg.len(),
        );
    }
    // exit(42) → 父 wait 收回 42
    unsafe {
        asm!(
            "ecall",
            in("a7") SYS_EXIT,
            in("a0") 42usize,
            options(noreturn),
        );
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // 子程序不应 panic；防御性退出码 1（父 wait 可见异常）
    unsafe {
        asm!(
            "ecall",
            in("a7") SYS_EXIT,
            in("a0") 1usize,
            options(noreturn),
        );
    }
}
