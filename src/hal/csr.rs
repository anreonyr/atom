// CSR 寄存器抽象层 — 类型安全的 RISC-V 控制/状态寄存器访问
//
// 所有 CSR 访问均在 S-mode 下完成，infallible。每个函数标记 #[inline(always)]
// 以保证编译为单条 csr 指令，零函数调用开销。
//
// 用法：
//   use crate::hal::csr::sie;
//   unsafe { sie::set(sie::SEIE); }

//
// 标志位分布：
//   SIE  (bit 1)       — 监管者全局中断使能
//   SPIE (bit 5)       — 进入陷阱前的 SIE 值 (sret 时恢复)
//   SPP  (bit 8)       — 进入陷阱前的特权模式 (0=User, 1=Supervisor)
//
// 用法:
//   use crate::hal::csr::sstatus::{self, Sstatus};
//   unsafe { sstatus::set(Sstatus::SIE); }
//   let ss = sstatus::read();
//   if ss.contains(Sstatus::SPIE) { ... }

pub mod sstatus {
    use core::arch::asm;

    bitflags! {
        /// sstatus 单比特标志位
        pub struct Sstatus: usize {
            /// SIE — 监管者全局中断使能 (bit 1)
            const SIE  = 1 << 1;
            /// SPIE — 进入陷阱前的 SIE 值 (bit 5), sret 时恢复
            const SPIE = 1 << 5;
        }
    }

    /// SPP (Supervisor Previous Privilege) 字段 (bit 8)
    ///
    /// 0 = User mode, 1 = Supervisor mode（单比特，不同于 M-mode 的 MPP 双比特）
    pub const SPP: usize = 1 << 8;

    /// 读取 sstatus 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> Sstatus {
        let r: usize;
        asm!("csrr {}, sstatus", out(reg) r);
        Sstatus::from_bits(r)
    }

    /// 写入 sstatus 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（当前用 set/clear/read）
    pub unsafe fn write(val: Sstatus) {
        asm!("csrw sstatus, {}", in(reg) val.bits());
    }

    /// 原子置位 — csrs (read-modify-write OR)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn set(bits: Sstatus) {
        asm!("csrs sstatus, {}", in(reg) bits.bits());
    }

    /// 原子清位 — csrc (read-modify-write AND NOT)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn clear(bits: Sstatus) {
        asm!("csrc sstatus, {}", in(reg) bits.bits());
    }
}

//
// 用法:
//   use crate::hal::csr::sie::{self, Sie};
//   unsafe { sie::set(Sie::SEIE); }

pub mod sie {
    use core::arch::asm;

    bitflags! {
        /// sie 中断使能标志位
        pub struct Sie: usize {
            /// SSIE — 监管者软件中断使能 (bit 1)
            const SSIE = 1 << 1;
            /// STIE — 监管者定时器中断使能 (bit 5)
            const STIE = 1 << 5;
            /// SEIE — 监管者外部中断使能 (bit 9)
            const SEIE = 1 << 9;
        }
    }

    /// 读取 sie 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（当前用 set）
    pub unsafe fn read() -> Sie {
        let r: usize;
        asm!("csrr {}, sie", out(reg) r);
        Sie::from_bits(r)
    }

    /// 写入 sie 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（当前用 set）
    pub unsafe fn write(val: Sie) {
        asm!("csrw sie, {}", in(reg) val.bits());
    }

    /// 原子置位 — csrs
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn set(bits: Sie) {
        asm!("csrs sie, {}", in(reg) bits.bits());
    }

    /// 原子清位 — csrc
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（当前用 set）
    pub unsafe fn clear(bits: Sie) {
        asm!("csrc sie, {}", in(reg) bits.bits());
    }
}

//
// 存放陷阱处理函数入口地址。MODE=0 为 Direct 模式。

pub mod stvec {
    use core::arch::asm;

    /// 写入陷阱向量基地址 (MODE=0 Direct)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(addr: usize) {
        asm!("csrw stvec, {}", in(reg) addr);
    }
}

//
// 位布局：
//   bit 63       — Interrupt flag (1 = 中断, 0 = 同步异常)
//   bits 0-10    — 异常/中断编号
//   bits 11-62   — 保留 (WPRI)
//
// 用法:
//   use crate::hal::csr::scause::{self, Scause};
//   let cause = scause::read();
//   if cause.contains(Scause::INTERRUPT) { ... }
//   match cause.code() { 1 => ..., 5 => ..., 9 => ... }

pub mod scause {
    use core::arch::asm;

    bitflags! {
        /// scause 标志位
        pub struct Scause: usize {
            /// 中断标志 (bit 63) — 1 = 异步中断, 0 = 同步异常
            const INTERRUPT = 1 << 63;
        }
    }

    /// 异常/中断编号 (bits 0-10, 共 11 位)
    pub const CODE_MASK: usize = 0x7FF;

    impl Scause {
        #[inline(always)]
        pub fn is_interrupt(self) -> bool {
            self.bits() & Scause::INTERRUPT.bits() != 0
        }
        /// 提取异常/中断编号 (bits 0-10)
        #[inline(always)]
        pub fn code(self) -> usize {
            self.bits() & CODE_MASK
        }
    }

    /// 读取 scause 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> Scause {
        let r: usize;
        asm!("csrr {}, scause", out(reg) r);
        Scause::from_bits(r)
    }

    /// 写入 scause 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（当前只读）
    pub unsafe fn write(val: Scause) {
        asm!("csrw scause, {}", in(reg) val.bits());
    }
}

//
// 存放发生异常/中断时的指令地址。sret 从 sepc 恢复执行。

pub mod sepc {
    use core::arch::asm;

    /// 读取异常 PC
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, sepc", out(reg) r);
        r
    }

    /// 写入异常 PC（用于异常返回前修改返回地址）
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    #[allow(dead_code)] // CSR 抽象完整性（trap_vector 直接 asm 写）
    pub unsafe fn write(val: usize) {
        asm!("csrw sepc, {}", in(reg) val);
    }
}

//
// 存放陷阱附加信息：
//   - 地址异常/缺页异常：故障地址
//   - 非法指令异常：指令编码
//   - 其他：0

pub mod stval {
    use core::arch::asm;

    /// 读取陷阱值
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, stval", out(reg) r);
        r
    }
}

//
// S-mode 下直接访问 satp 启用/禁用分页。
// 位布局：
//   MODE  (bits 63-60) — Sv39 = 8
//   ASID  (bits 59-44) — 地址空间 ID（单核下设为 0）
//   PPN   (bits 43-0)  — 根页表物理页号

pub mod satp {
    use core::arch::asm;

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
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, satp", out(reg) r);
        r
    }

    /// 写入 satp（启用分页 / 切换页表）
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
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
