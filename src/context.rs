// 任务上下文 — TrapFrame 布局与偏移常量
//
// trap_vector（trap.rs 的 naked asm）保存/恢复的寄存器帧。帧布局与汇编
// 保存序列的偏移通过本模块的编译期常量锁定：asm 侧以 `const` 操作数引用
// `FRAME_OFF_*` 与 `FRAME_SIZE`，任何字段顺序/类型/数量改动都会在编译期
// 体现为 asm 展开变化，杜绝"struct 与 asm 手工同步漂移"这类隐患。
//
// 依赖方向：本模块不依赖 trap/scheduler，是二者的共享底层（context ← trap，
// context ← scheduler），trap↔scheduler 的依赖环由此打破。

use core::mem::{offset_of, size_of};

/// 陷阱帧：trap_vector 保存/恢复的上下文（repr(C)，33 个 usize 字段 = 264 字节）。
///
/// 布局约束：
/// - `repr(C)`：字段顺序即内存布局，编译器不得重排；
/// - 全部 `usize` 字段：每字段 8 字节，偏移 = 序号 × 8；
/// - 尺寸锁定为 264（33 × 8）：asm 的栈帧开槽 `addi sp, sp, -{FRAME_SIZE}`
///   与本结构体必须一致，否则保存/恢复越界。
#[repr(C)]
pub struct TrapFrame {
    pub ra: usize,      // x1    offset 0
    pub sp: usize,      // x2    offset 8    ← 任务被打断前的原始 sp
    pub gp: usize,      // x3    offset 16
    pub tp: usize,      // x4    offset 24
    pub t0: usize,      // x5    offset 32
    pub t1: usize,      // x6    offset 40
    pub t2: usize,      // x7    offset 48
    pub s0: usize,      // x8    offset 56
    pub s1: usize,      // x9    offset 64
    pub a0: usize,      // x10   offset 72
    pub a1: usize,      // x11   offset 80
    pub a2: usize,      // x12   offset 88
    pub a3: usize,      // x13   offset 96
    pub a4: usize,      // x14   offset 104
    pub a5: usize,      // x15   offset 112
    pub a6: usize,      // x16   offset 120
    pub a7: usize,      // x17   offset 128
    pub s2: usize,      // x18   offset 136
    pub s3: usize,      // x19   offset 144
    pub s4: usize,      // x20   offset 152
    pub s5: usize,      // x21   offset 160
    pub s6: usize,      // x22   offset 168
    pub s7: usize,      // x23   offset 176
    pub s8: usize,      // x24   offset 184
    pub s9: usize,      // x25   offset 192
    pub s10: usize,     // x26   offset 200
    pub s11: usize,     // x27   offset 208
    pub t3: usize,      // x28   offset 216
    pub t4: usize,      // x29   offset 224
    pub t5: usize,      // x30   offset 232
    pub t6: usize,      // x31   offset 240
    pub sepc: usize,    // CSR   offset 248
    pub sstatus: usize, // CSR   offset 256
}

/// 帧尺寸（字节）。naked asm 以 `addi sp, sp, -{FRAME_SIZE}` 开槽，
/// 编译期从结构体推导，与布局永远一致。
pub const FRAME_SIZE: usize = size_of::<TrapFrame>();

/// 编译期锁定：帧尺寸必须是 33 × 8 = 264 字节（历史 naked asm 帧槽尺寸）。
/// 新增/删除/改动字段类型导致尺寸漂移时，此处编译失败，迫使同步审视 asm。
const _: () = assert!(
    FRAME_SIZE == 264,
    "TrapFrame size changed from 264 bytes — naked asm frame slot must be re-verified"
);

// ── 字段偏移常量（naked asm 保存/恢复序列引用，offset_of! 编译期派生）──
// 命名 `FRAME_OFF_<寄存器名>`：与 asm 行一一对应，便于对照检查。

/// offset_of!(TrapFrame, ra) — x1
pub const FRAME_OFF_RA: usize = offset_of!(TrapFrame, ra);
/// offset_of!(TrapFrame, sp) — x2（原始栈指针）
pub const FRAME_OFF_SP: usize = offset_of!(TrapFrame, sp);
/// offset_of!(TrapFrame, gp) — x3
pub const FRAME_OFF_GP: usize = offset_of!(TrapFrame, gp);
/// offset_of!(TrapFrame, tp) — x4
pub const FRAME_OFF_TP: usize = offset_of!(TrapFrame, tp);
/// offset_of!(TrapFrame, t0) — x5
pub const FRAME_OFF_T0: usize = offset_of!(TrapFrame, t0);
/// offset_of!(TrapFrame, t1) — x6
pub const FRAME_OFF_T1: usize = offset_of!(TrapFrame, t1);
/// offset_of!(TrapFrame, t2) — x7
pub const FRAME_OFF_T2: usize = offset_of!(TrapFrame, t2);
/// offset_of!(TrapFrame, s0) — x8
pub const FRAME_OFF_S0: usize = offset_of!(TrapFrame, s0);
/// offset_of!(TrapFrame, s1) — x9
pub const FRAME_OFF_S1: usize = offset_of!(TrapFrame, s1);
/// offset_of!(TrapFrame, a0) — x10
pub const FRAME_OFF_A0: usize = offset_of!(TrapFrame, a0);
/// offset_of!(TrapFrame, a1) — x11
pub const FRAME_OFF_A1: usize = offset_of!(TrapFrame, a1);
/// offset_of!(TrapFrame, a2) — x12
pub const FRAME_OFF_A2: usize = offset_of!(TrapFrame, a2);
/// offset_of!(TrapFrame, a3) — x13
pub const FRAME_OFF_A3: usize = offset_of!(TrapFrame, a3);
/// offset_of!(TrapFrame, a4) — x14
pub const FRAME_OFF_A4: usize = offset_of!(TrapFrame, a4);
/// offset_of!(TrapFrame, a5) — x15
pub const FRAME_OFF_A5: usize = offset_of!(TrapFrame, a5);
/// offset_of!(TrapFrame, a6) — x16
pub const FRAME_OFF_A6: usize = offset_of!(TrapFrame, a6);
/// offset_of!(TrapFrame, a7) — x17
pub const FRAME_OFF_A7: usize = offset_of!(TrapFrame, a7);
/// offset_of!(TrapFrame, s2) — x18
pub const FRAME_OFF_S2: usize = offset_of!(TrapFrame, s2);
/// offset_of!(TrapFrame, s3) — x19
pub const FRAME_OFF_S3: usize = offset_of!(TrapFrame, s3);
/// offset_of!(TrapFrame, s4) — x20
pub const FRAME_OFF_S4: usize = offset_of!(TrapFrame, s4);
/// offset_of!(TrapFrame, s5) — x21
pub const FRAME_OFF_S5: usize = offset_of!(TrapFrame, s5);
/// offset_of!(TrapFrame, s6) — x22
pub const FRAME_OFF_S6: usize = offset_of!(TrapFrame, s6);
/// offset_of!(TrapFrame, s7) — x23
pub const FRAME_OFF_S7: usize = offset_of!(TrapFrame, s7);
/// offset_of!(TrapFrame, s8) — x24
pub const FRAME_OFF_S8: usize = offset_of!(TrapFrame, s8);
/// offset_of!(TrapFrame, s9) — x25
pub const FRAME_OFF_S9: usize = offset_of!(TrapFrame, s9);
/// offset_of!(TrapFrame, s10) — x26
pub const FRAME_OFF_S10: usize = offset_of!(TrapFrame, s10);
/// offset_of!(TrapFrame, s11) — x27
pub const FRAME_OFF_S11: usize = offset_of!(TrapFrame, s11);
/// offset_of!(TrapFrame, t3) — x28
pub const FRAME_OFF_T3: usize = offset_of!(TrapFrame, t3);
/// offset_of!(TrapFrame, t4) — x29
pub const FRAME_OFF_T4: usize = offset_of!(TrapFrame, t4);
/// offset_of!(TrapFrame, t5) — x30
pub const FRAME_OFF_T5: usize = offset_of!(TrapFrame, t5);
/// offset_of!(TrapFrame, t6) — x31
pub const FRAME_OFF_T6: usize = offset_of!(TrapFrame, t6);
/// offset_of!(TrapFrame, sepc) — CSR sepc
pub const FRAME_OFF_SEPC: usize = offset_of!(TrapFrame, sepc);
/// offset_of!(TrapFrame, sstatus) — CSR sstatus
pub const FRAME_OFF_SSTATUS: usize = offset_of!(TrapFrame, sstatus);
