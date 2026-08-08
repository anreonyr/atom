// 用户程序验收探针 — M1 ELF loader + M2 时间 + M3 文件元数据 + M4 文件持久化
//
// 无 libc、无运行时：入口 `_start` 直接发 syscall（ecall 陷入内核）。以 ET_EXEC
// 静态链接（link.ld 基址 0x10000），loader 逐段映射后 sret 进入 U-mode 从这里
// 开始执行。验收流程（每步打标记，全过 exit(0)，任一步失败 exit(1) 供父任务
// wait 看出）：
//   [M1] hello from elf — loader 冒烟（延续 M1 探针）
//   [M2] gettimeofday 两时点差值非零（循环计时）
//   [M3] fstat(/dev/console0) 得 ByteDevice 类型（=2）
//   [M3] readdir(/dev) 列出节点名
//   [M4] /data/msg.txt：不存在 → create+写；存在 → 读回校验一致
//        （Boot1 写 → 重启 → Boot2 读回 = 持久性验收）
#![no_std]
#![no_main]

use core::arch::asm;
use core::hint::black_box;

// ── syscall 号（与内核 runtime/envcall.rs 常量一致） ──────
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const SYS_OPEN: usize = 1002;
const SYS_CLOSE: usize = 1003;
const SYS_GETTIMEOFDAY: usize = 1006;
const SYS_FSTAT: usize = 1007;
const SYS_READDIR: usize = 1008;
const SYS_CREATE: usize = 1009;

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

fn open(path: &str, flags: usize) -> isize {
    unsafe { syscall3(SYS_OPEN, path.as_ptr() as usize, flags, 0) }
}

fn write_str(fd: usize, s: &[u8]) {
    unsafe { syscall3(SYS_WRITE, fd, s.as_ptr() as usize, s.len()) };
}

fn gettimeofday(tv: &mut TimeVal) -> isize {
    unsafe { syscall3(SYS_GETTIMEOFDAY, tv as *mut TimeVal as usize, 0, 0) }
}

fn fstat(fd: usize, st: &mut Stat) -> isize {
    unsafe { syscall3(SYS_FSTAT, fd, st as *mut Stat as usize, 0) }
}

fn readdir(fd: usize, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_READDIR, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

fn read(fd: usize, buf: &mut [u8]) -> isize {
    unsafe { syscall3(SYS_READ, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

fn create(path: &str, flags: usize) -> isize {
    unsafe { syscall3(SYS_CREATE, path.as_ptr() as usize, flags, 0) }
}

fn close(fd: usize) -> isize {
    unsafe { syscall3(SYS_CLOSE, fd, 0, 0) }
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

/// 用户侧 stat（与内核 sys_fstat 写回布局一致：2×u64 type, size）
#[repr(C)]
struct Stat {
    file_type: u64,
    size: u64,
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

    // ── M3a：open("/dev/console0") → fstat → ByteDevice(2) ──
    let fd = open("/dev/console0\0", 0); // O_RDONLY
    if fd < 0 {
        ok = false;
        write_str(1, b"[M3] open /dev/console0 failed\n");
    } else {
        let mut st = Stat { file_type: 0, size: 0 };
        let r = fstat(fd as usize, &mut st);
        if r < 0 || st.file_type != 2 {
            ok = false;
            write_str(1, b"[M3] fstat console0 failed (type=");
            print_dec(st.file_type);
            write_str(1, b")\n");
        } else {
            write_str(1, b"[M3] fstat console0 type=2 size=");
            print_dec(st.size);
            write_str(1, b"\n");
        }
    }

    // ── M3b：open("/dev") → readdir → 列节点名 ──
    let dfd = open("/dev\0", 0);
    if dfd < 0 {
        ok = false;
        write_str(1, b"[M3] open /dev failed\n");
    } else {
        let mut buf = [0u8; 256];
        let n = readdir(dfd as usize, &mut buf);
        if n < 0 {
            ok = false;
            write_str(1, b"[M3] readdir /dev failed\n");
        } else {
            write_str(1, b"[M3] readdir /dev:\n");
            write_str(1, &buf[..n as usize]);
        }
    }

    // ── M4：文件持久化 — /data/msg.txt（file::fs 动态目录）──
    //  Boot1：open → ENOENT → create+写 → exit；重启后 Boot2：open → 读回校验。
    //  disk.img 持久于 host，两次 QEMU 启动即验收持久性。
    let content = b"hello atom persistent\n";
    let fd = open("/data/msg.txt\0", 2); // O_RDWR
    if fd < 0 {
        // 不存在（Boot1）→ create → 写
        let cfd = create("/data/msg.txt\0", 2);
        if cfd < 0 {
            ok = false;
            write_str(1, b"[M4] create /data/msg.txt failed\n");
        } else {
            write_str(cfd as usize, content);
            if close(cfd as usize) != 0 {
                ok = false;
                write_str(1, b"[M4] close failed\n");
            } else {
                write_str(1, b"[M4] boot1 wrote /data/msg.txt (restart to verify)\n");
            }
        }
    } else {
        // 已存在（Boot2）→ 读回比对
        let mut buf = [0u8; 64];
        let n = read(fd as usize, &mut buf);
        let _ = close(fd as usize);
        if n == content.len() as isize && &buf[..n as usize] == content {
            write_str(1, b"[M4] boot2 readback consistent\n");
        } else {
            ok = false;
            write_str(1, b"[M4] readback MISMATCH (n=");
            print_dec(if n < 0 { 0 } else { n as u64 });
            write_str(1, b")\n");
        }
    }

    exit(if ok { 0 } else { 1 });
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(1)
}
