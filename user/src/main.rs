// 用户程序验收探针 — M1 ELF loader + M2 时间 syscall
//
// 无 libc、无运行时：入口 `_start` 直接发 syscall（ecall 陷入内核）。以 ET_EXEC
// 静态链接（link.ld 基址 0x10000），loader 逐段映射后 sret 进入 U-mode 从这里
// 开始执行。验收流程（M3 文件元数据 syscall 在下一里程碑加入）：
//   [M1] hello from elf — loader 冒烟
//   [M2] gettimeofday 两时点差值非零（循环计时）
// 全过 exit(0)，任一步失败 exit(1) 供父任务 wait 看出。
#![no_std]
#![no_main]

use core::arch::asm;
use core::hint::black_box;

// ── syscall 号（与内核 runtime/envcall.rs 常量一致） ──────
const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const SYS_GETTIMEOFDAY: usize = 1006;

/// 通用 syscall（a7 号 + a0..a2 参数）。
///
/// 内核 trap 帧保存/恢复全部 GPR，只写回 a0 并 sepc+=4——故只需 `inout a0`，
/// 不声明 `clobber_abi`（声明了反而与 in 操作数重叠触发 asm 校验错误，且内核
/// 实际保留全部寄存器）。
#[inline]
unsafe fn syscall3(n: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let mut a0 = a0;
    unsafe {
        asm!(
            "ecall",
            in("a7") n,
            inout("a0") a0,
            in("a1") a1,
            in("a2") a2,
        );
    }
    a0 as isize
}

fn write_str(fd: usize, s: &[u8]) {
    unsafe { syscall3(SYS_WRITE, fd, s.as_ptr() as usize, s.len()) };
}

fn gettimeofday(tv: &mut TimeVal) -> isize {
    unsafe { syscall3(SYS_GETTIMEOFDAY, tv as *mut TimeVal as usize, 0, 0) }
}

fn exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a7") SYS_EXIT, in("a0") code, options(noreturn)) }
}

/// 用户侧 timeval（与内核 sys_gettimeofday 写回布局一致：2×u64 sec, usec）
#[repr(C)]
struct TimeVal {
    sec: u64,
    usec: u64,
}

/// 十进制打印（无 libc）：栈上逆序逐位，一次性 write 输出。
fn print_dec(mut n: u64) {
    let mut buf = [0u8; 20];
    if n == 0 {
        write_str(1, b"0");
        return;
    }
    let mut len = 0;
    while n > 0 {
        buf[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    let mut out = [0u8; 20];
    let mut j = 0;
    while len > 0 {
        len -= 1;
        out[j] = buf[len];
        j += 1;
    }
    write_str(1, &out[..j]);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
extern "C" fn _start() -> ! {
    let mut ok = true;

    // M1 冒烟：loader 装载可见
    write_str(1, b"hello from elf\n");

    // ── M2：gettimeofday ×2（间隔忙循环）→ 差值非零 ──
    let mut t1 = TimeVal { sec: 0, usec: 0 };
    let mut t2 = TimeVal { sec: 0, usec: 0 };
    if gettimeofday(&mut t1) < 0 {
        ok = false;
        write_str(1, b"[M2] gettimeofday(t1) failed\n");
    }
    // 忙循环制造时间间隔；black_box 防 release 优化掉（否则可能 diff=0 误判）
    let mut acc = 0usize;
    for _ in 0..1_000_000 {
        acc = acc.wrapping_add(1);
    }
    black_box(acc);
    if gettimeofday(&mut t2) < 0 {
        ok = false;
        write_str(1, b"[M2] gettimeofday(t2) failed\n");
    }
    let diff = t2
        .sec
        .wrapping_sub(t1.sec)
        .wrapping_mul(1_000_000)
        .wrapping_add(t2.usec.wrapping_sub(t1.usec));
    if diff == 0 {
        ok = false;
        write_str(1, b"[M2] diff_usec=0 (busy loop too fast?)\n");
    } else {
        write_str(1, b"[M2] diff_usec=");
        print_dec(diff);
        write_str(1, b"\n");
    }

    exit(if ok { 0 } else { 1 });
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(1)
}
