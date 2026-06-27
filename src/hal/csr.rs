// CSR 寄存器抽象层 — 类型安全的 RISC-V 控制/状态寄存器访问
//
// 所有 CSR 访问均在 M-mode 下完成，infallible。每个函数标记 #[inline(always)]
// 以保证编译为单条 csr 指令，零函数调用开销。
//
// 用法：
//   use crate::hal::csr::mie;
//   unsafe { mie::set(mie::MEIE); }

// ── mstatus (Machine Status Register, 0x300) ──────────────
//
// 关键位：
//   MIE  (bit 3)      — 机器全局中断使能
//   MPIE (bit 7)      — 进入陷阱前的 MIE 值 (mret 时恢复)
//   MPP  (bits 11-12) — 进入陷阱前的特权模式

pub mod mstatus {
    use core::arch::asm;

    /// MIE (Machine Interrupt Enable) — 位 3
    pub const MIE: usize = 1 << 3;
    /// MPIE (Machine Previous Interrupt Enable) — 位 7
    pub const MPIE: usize = 1 << 7;
    /// MPP (Machine Previous Privilege) — 位 11-12
    pub const MPP: usize = 0b11 << 11;

    /// 读取 mstatus
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mstatus", out(reg) r);
        r
    }

    /// 写入 mstatus
    #[inline(always)]
    pub unsafe fn write(val: usize) {
        asm!("csrw mstatus, {}", in(reg) val);
    }

    /// 置位 — csrs (atomic read-modify-write OR)
    #[inline(always)]
    pub unsafe fn set(bits: usize) {
        asm!("csrs mstatus, {}", in(reg) bits);
    }

    /// 清除位 — csrc (atomic read-modify-write AND NOT)
    #[inline(always)]
    pub unsafe fn clear(bits: usize) {
        asm!("csrc mstatus, {}", in(reg) bits);
    }
}

// ── mie (Machine Interrupt Enable Register, 0x304) ────────
//
// 关键位：
//   MSIE (bit 3)  — 机器软件中断使能
//   MTIE (bit 7)  — 机器定时器中断使能
//   MEIE (bit 11) — 机器外部中断使能

pub mod mie {
    use core::arch::asm;

    /// MSIE (Machine Software Interrupt Enable) — 位 3
    pub const MSIE: usize = 1 << 3;
    /// MTIE (Machine Timer Interrupt Enable) — 位 7
    pub const MTIE: usize = 1 << 7;
    /// MEIE (Machine External Interrupt Enable) — 位 11
    pub const MEIE: usize = 1 << 11;

    /// 读取 mie
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mie", out(reg) r);
        r
    }

    /// 写入 mie
    #[inline(always)]
    pub unsafe fn write(val: usize) {
        asm!("csrw mie, {}", in(reg) val);
    }

    /// 置位 — csrs
    #[inline(always)]
    pub unsafe fn set(bits: usize) {
        asm!("csrs mie, {}", in(reg) bits);
    }

    /// 清除位 — csrc
    #[inline(always)]
    pub unsafe fn clear(bits: usize) {
        asm!("csrc mie, {}", in(reg) bits);
    }
}

// ── mtvec (Machine Trap Vector Register, 0x305) ───────────
//
// 存放陷阱处理函数入口地址。MODE=0 为 Direct 模式。

pub mod mtvec {
    use core::arch::asm;

    /// 写入陷阱向量基地址 (MODE=0 Direct)
    #[inline(always)]
    pub unsafe fn write(addr: usize) {
        asm!("csrw mtvec, {}", in(reg) addr);
    }
}

// ── mcause (Machine Cause Register, 0x342) ────────────────
//
// 位布局：
//   bit 63       — Interrupt flag (1 = 中断, 0 = 同步异常)
//   bits 0-10    — 异常/中断编号
//   bits 11-62   — 保留 (WPRI)

pub mod mcause {
    use core::arch::asm;

    /// 中断编号位宽 (0-10 共 11 位)
    pub const CODE_BITS: usize = 11;
    /// 中断编号掩码
    pub const CODE_MASK: usize = (1 << CODE_BITS) - 1;
    /// 中断标志位 (bit 63, RV64)
    pub const INTERRUPT: usize = 1 << 63;

    /// 读取 mcause 原始值
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mcause", out(reg) r);
        r
    }

    /// 写入 mcause
    #[inline(always)]
    pub unsafe fn write(val: usize) {
        asm!("csrw mcause, {}", in(reg) val);
    }

    /// 检查是否为中断 (bit 63 == 1)
    #[inline(always)]
    pub fn is_interrupt(val: usize) -> bool {
        val & INTERRUPT != 0
    }

    /// 提取异常/中断编号 (bits 0-10)
    #[inline(always)]
    pub fn code(val: usize) -> usize {
        val & CODE_MASK
    }
}

// ── mepc (Machine Exception Program Counter, 0x341) ───────
//
// 存放发生异常/中断时的指令地址。mret 从 mepc 恢复执行。

pub mod mepc {
    use core::arch::asm;

    /// 读取异常 PC
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mepc", out(reg) r);
        r
    }

    /// 写入异常 PC（用于异常返回前修改返回地址）
    #[inline(always)]
    pub unsafe fn write(val: usize) {
        asm!("csrw mepc, {}", in(reg) val);
    }
}

// ── mtval (Machine Trap Value Register, 0x343) ────────────
//
// 存放陷阱附加信息：
//   - 地址异常/缺页异常：故障地址
//   - 非法指令异常：指令编码
//   - 其他：0

pub mod mtval {
    use core::arch::asm;

    /// 读取陷阱值
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mtval", out(reg) r);
        r
    }
}

// ── satp (Supervisor Address Translation and Protection, 0x180) ──
//
// M-mode 下直接访问 satp 启用/禁用分页。
// 位布局：
//   MODE  (bits 63-60) — Sv39 = 8
//   ASID  (bits 59-44) — 地址空间 ID（单核下设为 0）
//   PPN   (bits 43-0)  — 根页表物理页号

pub mod satp {
    use core::arch::asm;

    /// MODE: Bare（无地址翻译）
    pub const MODE_BARE: usize = 0;
    /// MODE: Sv39（三级页表）
    pub const MODE_SV39: usize = 8;

    const MODE_SHIFT: usize = 60;

    /// 构造 satp 值: MODE | (ASID << 44) | PPN
    #[inline(always)]
    pub const fn make(mode: usize, asid: usize, ppn: usize) -> usize {
        (mode << MODE_SHIFT) | ((asid & 0xFFFF) << 44) | (ppn & 0x000F_FFFF_FFFF)
    }

    /// 读取 satp
    #[inline(always)]
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, satp", out(reg) r);
        r
    }

    /// 写入 satp（启用分页 / 切换页表）
    #[inline(always)]
    pub unsafe fn write(val: usize) {
        asm!("csrw satp, {}", in(reg) val);
    }

    /// 提取 MODE 字段
    #[inline(always)]
    pub fn mode(val: usize) -> usize {
        val >> MODE_SHIFT
    }

    /// 提取根页表 PPN
    #[inline(always)]
    pub fn ppn(val: usize) -> usize {
        val & 0x000F_FFFF_FFFF
    }
}
