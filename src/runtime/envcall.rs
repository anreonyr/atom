// envcall 分发 — 处理来自 U-mode 的 ecall（scause=8，Environment Call from U-mode）
//
// U-mode 任务执行 ecall 指令 → 陷入 S-mode → trap_handler 按 scause=8 路由到
// 本模块（S-mode 自身的 ecall 是 scause=9，直接陷入 M-mode OpenSBI，不经这里）。
// 调用约定（RISC-V ABI + Linux 惯例）：
//   a7 = 调用号（syscall number）
//   a0..a6 = 参数
//   返回: a0 = 返回值；错误用负数 -errno 编码（usize 两补），未知号 → -ENOSYS
//
// 当前调用面：
//   read(fd, buf, count) — a7=63（Linux riscv64 号）；fd 仅支持 0（stdin →
//   console 输入）。等待机制：trap 上下文 SIE=0 不能 wfi，缓冲空时置
//   WaitRead 返回，trap_handler 检测到不加 sepc → sret 重放本 ecall（用户
//   态忙转直到 tick 抢占 park / 字符到达），读到后复位标记 +4 正常返回。
// 其余调用（write/exit 等）后续在此 match 中按号扩展。

/// Linux errno ENOSYS = 38；返回值语义为 -errno（负数），usize 下取两补。
pub const ENOSYS: usize = (38usize).wrapping_neg();
/// Linux riscv64 系统调用号：read = 63。
pub const READ: usize = 63;
/// Linux riscv64 系统调用号：write = 64。
pub const WRITE: usize = 64;
/// Linux riscv64 系统调用号：exit = 93。
pub const EXIT: usize = 93;
/// errno EBADF = 9（fd 非法）
const EBADF: usize = (9usize).wrapping_neg();
/// errno ENODEV = 19（无输入设备）
const ENODEV: usize = (19usize).wrapping_neg();
/// errno EAGAIN = 11（缓冲空，等待重放）
const EAGAIN: usize = (11usize).wrapping_neg();
/// errno EFAULT = 14（用户指针非法：非用户区或未映射）
const EFAULT: usize = (14usize).wrapping_neg();

use crate::context::TrapFrame;
use crate::file::FileError;
use crate::{debug, info};

/// envcall 分发结果 — trap_handler 据此决定"写回 a0 恢复用户态"还是
/// "任务已终止（exit）直接恢复下一任务"。
pub enum DispatchResult {
    /// 正常返回：值写回 `frame.a0`；trap_handler 按 [`crate::schedule::is_input_waiting`]
    /// 决定 sepc+4 正常返回或重放 ecall（read 等待机制）。
    Ret(usize),
    /// 任务已终止（exit syscall）：值为下一 TrapFrame 指针，trap_handler 直接
    /// 恢复——不再写 a0、不再加 sepc。
    Terminate(usize),
}

/// 分发 U-mode ecall。
///
/// `frame` = 当前任务已保存的 TrapFrame（exit 终止路径需要）；`number` = a7
/// （调用号），`args` = a0..a5（参数寄存器快照）。正常返回经 [`DispatchResult::Ret`]
/// 由 trap_handler 写回 a0；exit 经 [`DispatchResult::Terminate`] 返回下一帧。
/// 未知号返回 [`ENOSYS`]。
///
/// # 调用面
///
/// - `READ`（63）：`read(fd, buf, count)`。fd 仅支持 0（stdin → preferred
///   输入设备）；buf 须为映射的**用户区地址**（校验失败 -EFAULT）。
///   缓冲空 → 置输入等待（WaitRead）返回 -EAGAIN——trap_handler 检测到
///   WaitRead 不加 sepc，sret 重放 ecall（用户态忙转直到 tick 抢占 park，
///   字符到达唤醒后再重放读到）；缓冲有数据 → 读到后复位标记正常返回。
///   无输入设备 → -ENODEV；fd ≠ 0 → -EBADF；count = 0 → 0。
/// - `WRITE`（64）：`write(fd, buf, count)`。fd 仅支持 1（stdout → preferred
///   输出设备）；buf 须为映射的**用户区地址**（校验失败 -EFAULT），内容按
///   文本输出（教学取舍：非 UTF-8 字节丢弃，见 [`write`] 分支）。
///   返回写入字节数；fd ≠ 1 → -EBADF；count = 0 → 0。
/// - `EXIT`（93）：`exit(code)`。标 Zombie（带退出码）后 trap 态直接
///   dispatch 下一任务——不再恢复用户态。
///
/// # 日志
///
/// READ 走 debug!——等待期间每轮重放都进本分发，info! 会刷屏；未知号保留
/// info!（骨架阶段可见性优先）。
pub fn dispatch(frame: *mut TrapFrame, number: usize, args: [usize; 6]) -> DispatchResult {
    match number {
        READ => {
            let fd = args[0];
            if fd != 0 {
                debug!("envcall: read(fd={fd}) → -EBADF");
                return DispatchResult::Ret(EBADF); // 仅 stdin（console 输入）
            }
            let buf = args[1] as *mut u8;
            let count = args[2];
            if count == 0 {
                return DispatchResult::Ret(0);
            }
            // 用户 buf 校验：非用户区或未映射 → -EFAULT（防止写坏内核）
            if !user_ptr_valid(buf as usize, count) {
                debug!("envcall: read(buf={buf:p} count={count}) → -EFAULT");
                return DispatchResult::Ret(EFAULT);
            }
            match unsafe { crate::input::read_to(buf, count) } {
                Ok(n) => {
                    // 读到数据：复位输入等待标记（若重放循环期间置过位）——
                    // trap_handler 据此 +4 正常返回。
                    crate::schedule::clear_input_wait();
                    DispatchResult::Ret(n)
                }
                Err(FileError::WouldBlock) => {
                    // 缓冲空：置输入等待（trap_handler 检测 WaitRead → 不加
                    // sepc，sret 重放 ecall 继续等；tick 抢占 park 后由字符
                    // 到达唤醒，重放读到）。
                    crate::schedule::mark_input_wait();
                    DispatchResult::Ret(EAGAIN) // 用户不可见（被重放）；读到数据才 +4 返回
                }
                Err(_) => DispatchResult::Ret(ENODEV),
            }
        }
        WRITE => {
            let (fd, buf, count) = (args[0], args[1], args[2]);
            if fd != 1 {
                debug!("envcall: write(fd={fd}) → -EBADF");
                return DispatchResult::Ret(EBADF); // 仅 stdout（console 输出）
            }
            if count == 0 {
                return DispatchResult::Ret(0);
            }
            // 用户 buf 校验：非用户区或未映射 → -EFAULT（防止读坏内核/读野指针）
            if !user_ptr_valid(buf, count) {
                debug!("envcall: write(buf={buf:#x} count={count}) → -EFAULT");
                return DispatchResult::Ret(EFAULT);
            }
            // 从用户 buf 读 count 字节输出到 preferred 输出设备。教学取舍：
            // 按文本处理（非 UTF-8 字节丢弃）——console 设备层是 fmt::Write。
            // SAFETY: 校验已保证 [buf, buf+count) 落在用户区且每页映射。
            let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
            let s = core::str::from_utf8(bytes).unwrap_or("");
            let w = crate::sink::current_mut();
            let _ = core::fmt::Write::write_str(w, s);
            DispatchResult::Ret(count)
        }
        EXIT => {
            let code = args[0] as i32;
            info!("envcall: exit({code})");
            // 标 Zombie（带退出码）后 trap 态直接 dispatch——不再恢复用户态。
            // 与缺页 terminate 共用路径，区别仅退出码。
            DispatchResult::Terminate(crate::schedule::terminate_current(frame, code))
        }
        _ => {
            let ret = ENOSYS;
            info!("envcall: number={number:#x} args={args:?} → {ret:#x}");
            DispatchResult::Ret(ret)
        }
    }
}

/// 校验用户指针 `[addr, addr+len)` 可访问：落在用户半区且每页均已在
/// 当前任务空间映射。
///
/// 单 hart 关中断（trap 上下文）下校验与后续访问之间页表不会变化
/// （缺页处理也发生在同一 hart），无 TOCTOU 竞态。
fn user_ptr_valid(addr: usize, len: usize) -> bool {
    let Some(sp) = crate::schedule::current_space() else {
        return false; // 无当前任务空间（boot/空闲）——用户指针必然非法
    };
    // SAFETY: current_space 返回的 NonNull 指向 CURRENT 任务的空间，
    // trap 期间不回收（见 trap.rs ecall 分支注释）。
    let space = unsafe { sp.as_ref() };
    let start = crate::memory::addr::VirtAddr::from_raw(addr);
    if !start.is_user() {
        return false;
    }
    // 覆盖 len 的每一页（含部分页）：任一页未映射 → 非法
    let end = addr.saturating_add(len);
    let mut va = start;
    while va.as_usize() < end {
        if space.translate(va).is_none() {
            return false;
        }
        va = va + crate::memory::PAGE_SIZE;
    }
    true
}
