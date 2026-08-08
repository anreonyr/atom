// 用户程序验收探针 — M1 ELF loader 的加载目标
//
// 无 libc、无运行时：入口 `_start` 直接发 syscall（ecall 陷入内核）。
// 以 ET_EXEC 静态链接（link.ld 基址 0x10000），loader 逐段映射后
// sret 进入 U-mode 从这里开始执行。写完 stdout 后 exit(0) 干净退出，
// 父任务经 wait 收回退出码——ROADMAP M1 的验收闭环。
#![no_std]
#![no_main]

use core::arch::global_asm;

global_asm!(
    ".section .text._start",
    ".globl _start",
    "_start:",
    // write(1, msg, 15) → stdout（fd 1 = /dev/console0，envcall 预置）
    "  li    a0, 1",
    "  la    a1, 1f", // msg 地址（ET_EXEC 静态链接，绝对地址 = 装载 VMA）
    "  li    a2, 15", // len = "hello from elf\n"
    "  li    a7, 64", // WRITE
    "  ecall",
    // exit(0) → 标 Zombie（退出码 0），父 wait 取回
    "  li    a0, 0",
    "  li    a7, 93", // EXIT
    "  ecall", // 不返回
    "1:",
    "  .ascii \"hello from elf\\n\"",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
