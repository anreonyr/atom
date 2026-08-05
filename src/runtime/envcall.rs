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
/// errno EBADF = 9（fd 非法）
const EBADF: usize = (9usize).wrapping_neg();
/// errno ENODEV = 19（无输入设备）
const ENODEV: usize = (19usize).wrapping_neg();
/// errno EAGAIN = 11（缓冲空，等待重放）
const EAGAIN: usize = (11usize).wrapping_neg();

use crate::file::FileError;
use crate::{debug, info};

/// 分发 U-mode ecall。
///
/// `number` = a7（调用号），`args` = a0..a6（参数寄存器快照）。返回值由
/// trap_handler 写回 frame.a0。未知号返回 [`ENOSYS`]。
///
/// # 调用面
///
/// - `READ`（63）：`read(fd, buf, count)`。fd 仅支持 0（stdin → preferred
///   输入设备）；读入用户 buf（信任用户指针，SUM 已置位，S-mode 可写 U 页；
///   真实内核需 copy_to_user，骨架阶段从简）。缓冲空 → 置输入等待
///   （WaitRead）返回 -EAGAIN——trap_handler 检测到 WaitRead 不加 sepc，
///   sret 重放 ecall（用户态忙转直到 tick 抢占 park，字符到达唤醒后再重放
///   读到）；缓冲有数据 → 读到后复位标记正常返回。无输入设备 → -ENODEV；
///   fd ≠ 0 → -EBADF；count = 0 → 0。
///
/// # 日志
///
/// READ 走 debug!——等待期间每轮重放都进本分发，info! 会刷屏；未知号保留
/// info!（骨架阶段可见性优先）。
pub fn dispatch(number: usize, args: [usize; 6]) -> usize {
    match number {
        READ => {
            let fd = args[0];
            if fd != 0 {
                debug!("envcall: read(fd={fd}) → -EBADF");
                return EBADF; // 仅 stdin（console 输入）
            }
            let buf = args[1] as *mut u8;
            let count = args[2];
            if count == 0 {
                return 0;
            }
            match unsafe { crate::input::read_to(buf, count) } {
                Ok(n) => {
                    // 读到数据：复位输入等待标记（若重放循环期间置过位）——
                    // trap_handler 据此 +4 正常返回。
                    crate::schedule::clear_input_wait();
                    n
                }
                Err(FileError::WouldBlock) => {
                    // 缓冲空：置输入等待（trap_handler 检测 WaitRead → 不加
                    // sepc，sret 重放 ecall 继续等；tick 抢占 park 后由字符
                    // 到达唤醒，重放读到）。
                    crate::schedule::mark_input_wait();
                    EAGAIN // 用户不可见（被重放）；读到数据才 +4 返回
                }
                Err(_) => ENODEV,
            }
        }
        _ => {
            let ret = ENOSYS;
            info!("envcall: number={number:#x} args={args:?} → {ret:#x}");
            ret
        }
    }
}
