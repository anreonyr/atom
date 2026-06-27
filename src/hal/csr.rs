// CSR 寄存器抽象层 — 类型安全的 RISC-V 控制/状态寄存器访问
//
// 所有 CSR 访问均在 M-mode 下完成，infallible。每个函数标记 #[inline(always)]
// 以保证编译为单条 csr 指令，零函数调用开销。
//
// 用法：
//   use crate::hal::csr::mie;
//   unsafe { mie::set(mie::MEIE); }

//
// 标志位分布：
//   MIE  (bit 3)       — 机器全局中断使能
//   MPIE (bit 7)       — 进入陷阱前的 MIE 值 (mret 时恢复)
//   MPP  (bits 11-12)  — 进入陷阱前的特权模式 (多比特字段, 见 mpp 子模块)
//
// 用法:
//   use crate::hal::csr::mstatus::{self, Mstatus};
//   unsafe { mstatus::set(Mstatus::MIE); }
//   let ms = mstatus::read();
//   if ms.contains(Mstatus::MPIE) { ... }

pub mod mstatus {
    use core::arch::asm;

    bitflags! {
        /// mstatus 单比特标志位
        pub struct Mstatus: usize {
            /// MIE — 机器全局中断使能 (bit 3)
            const MIE  = 1 << 3;
            /// MPIE — 进入陷阱前的 MIE 值 (bit 7), mret 时恢复
            const MPIE = 1 << 7;
            /// MPRV — 修改特权级 (bit 17), 用于 M-mode 下以 U/S-mode 权限访问内存
            const MPRV = 1 << 17;
        }
    }

    /// MPP (Machine Previous Privilege) 多比特字段常量 (bits 11-12)
    ///
    /// 用法:
    ///   Mstatus::from_bits(mpp::M)           // 仅 MPP=M, 其他位全零
    ///   Mstatus::MPIE | Mstatus::from_bits(mpp::M)  // MPIE + MPP=M
    ///   val & Mstatus::from_bits(mpp::MASK) == Mstatus::from_bits(mpp::U)  // 检查 MPP
    pub mod mpp {
        /// MPP 掩码 (bits 11-12)
        pub const MASK: usize = 0b11 << 11;
        /// User mode
        pub const U: usize = 0b00 << 11;
        /// Supervisor mode
        pub const S: usize = 0b01 << 11;
        /// Machine mode
        pub const M: usize = 0b11 << 11;
    }

    /// 读取 mstatus 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> Mstatus {
        let r: usize;
        asm!("csrr {}, mstatus", out(reg) r);
        Mstatus::from_bits(r)
    }

    /// 写入 mstatus 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(val: Mstatus) {
        asm!("csrw mstatus, {}", in(reg) val.bits());
    }

    /// 原子置位 — csrs (read-modify-write OR)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn set(bits: Mstatus) {
        asm!("csrs mstatus, {}", in(reg) bits.bits());
    }

    /// 原子清位 — csrc (read-modify-write AND NOT)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn clear(bits: Mstatus) {
        asm!("csrc mstatus, {}", in(reg) bits.bits());
    }
}

//
// 用法:
//   use crate::hal::csr::mie::{self, Mie};
//   unsafe { mie::set(Mie::MEIE); }

pub mod mie {
    use core::arch::asm;

    bitflags! {
        /// mie 中断使能标志位
        pub struct Mie: usize {
            /// MSIE — 机器软件中断使能 (bit 3)
            const MSIE = 1 << 3;
            /// MTIE — 机器定时器中断使能 (bit 7)
            const MTIE = 1 << 7;
            /// MEIE — 机器外部中断使能 (bit 11)
            const MEIE = 1 << 11;
        }
    }

    /// 读取 mie 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> Mie {
        let r: usize;
        asm!("csrr {}, mie", out(reg) r);
        Mie::from_bits(r)
    }

    /// 写入 mie 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(val: Mie) {
        asm!("csrw mie, {}", in(reg) val.bits());
    }

    /// 原子置位 — csrs
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn set(bits: Mie) {
        asm!("csrs mie, {}", in(reg) bits.bits());
    }

    /// 原子清位 — csrc
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn clear(bits: Mie) {
        asm!("csrc mie, {}", in(reg) bits.bits());
    }
}

//
// 存放陷阱处理函数入口地址。MODE=0 为 Direct 模式。

pub mod mtvec {
    use core::arch::asm;

    /// 写入陷阱向量基地址 (MODE=0 Direct)
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(addr: usize) {
        asm!("csrw mtvec, {}", in(reg) addr);
    }
}

//
// 位布局：
//   bit 63       — Interrupt flag (1 = 中断, 0 = 同步异常)
//   bits 0-10    — 异常/中断编号
//   bits 11-62   — 保留 (WPRI)
//
// 用法:
//   use crate::hal::csr::mcause::{self, Mcause};
//   let cause = mcause::read();
//   if cause.contains(Mcause::INTERRUPT) { ... }
//   match cause.code() { 3 => ..., 7 => ..., 11 => ... }

pub mod mcause {
    use core::arch::asm;

    bitflags! {
        /// mcause 标志位
        pub struct Mcause: usize {
            /// 中断标志 (bit 63) — 1 = 异步中断, 0 = 同步异常
            const INTERRUPT = 1 << 63;
        }
    }

    /// 异常/中断编号 (bits 0-10, 共 11 位)
    pub const CODE_MASK: usize = 0x7FF;

    impl Mcause {
        #[inline(always)]
        pub fn is_interrupt(self) -> bool {
            self.bits() & Mcause::INTERRUPT.bits() != 0
        }
        /// 提取异常/中断编号 (bits 0-10)
        #[inline(always)]
        pub fn code(self) -> usize {
            self.bits() & CODE_MASK
        }
    }

    /// 读取 mcause 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> Mcause {
        let r: usize;
        asm!("csrr {}, mcause", out(reg) r);
        Mcause::from_bits(r)
    }

    /// 写入 mcause 寄存器
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(val: Mcause) {
        asm!("csrw mcause, {}", in(reg) val.bits());
    }
}

//
// 存放发生异常/中断时的指令地址。mret 从 mepc 恢复执行。

pub mod mepc {
    use core::arch::asm;

    /// 读取异常 PC
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mepc", out(reg) r);
        r
    }

    /// 写入异常 PC（用于异常返回前修改返回地址）
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn write(val: usize) {
        asm!("csrw mepc, {}", in(reg) val);
    }
}

//
// 存放陷阱附加信息：
//   - 地址异常/缺页异常：故障地址
//   - 非法指令异常：指令编码
//   - 其他：0

pub mod mtval {
    use core::arch::asm;

    /// 读取陷阱值
    #[inline(always)]
    ///
    /// # Safety
    /// 直接访问 CSR，调用者需确保在正确的特权级下操作。
    pub unsafe fn read() -> usize {
        let r: usize;
        asm!("csrr {}, mtval", out(reg) r);
        r
    }
}

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
