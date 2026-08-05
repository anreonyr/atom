// envcall 分发 — 处理来自 U-mode 的 ecall（scause=8，Environment Call from U-mode）
//
// U-mode 任务执行 ecall 指令 → 陷入 S-mode → trap_handler 按 scause=8 路由到
// 本模块（S-mode 自身的 ecall 是 scause=9，直接陷入 M-mode OpenSBI，不经这里）。
// 调用约定（RISC-V ABI + Linux 惯例）：
//   a7 = 调用号（syscall number）
//   a0..a6 = 参数
//   返回: a0 = 返回值；错误用负数 -errno 编码（usize 两补），未知号 → -ENOSYS
//
// 当前为入口骨架：分发表为空，未知号一律 -ENOSYS。具体系统调用
// （write/exit 等）后续在此 match 中按号扩展。

/// Linux errno ENOSYS = 38；返回值语义为 -errno（负数），usize 下取两补。
pub const ENOSYS: usize = (38usize).wrapping_neg();

use crate::info;

/// 分发 U-mode ecall。
///
/// `number` = a7（调用号），`args` = a0..a6（参数寄存器快照）。返回值由
/// trap_handler 写回 frame.a0。未知号返回 [`ENOSYS`]。
///
/// # 骨架说明
///
/// 参数 `args` 暂未使用（无具体调用），签名先行固定以便后续扩展。
/// 每次分发打 info! 日志（骨架阶段唯一 syscall 通道，可见性优先；
/// 具体调用落地后可降级 debug/trace 减噪）。
pub fn dispatch(number: usize, args: [usize; 6]) -> usize {
    let ret = ENOSYS;
    info!("envcall: number={number:#x} args={args:?} → {ret:#x}");
    ret
}
