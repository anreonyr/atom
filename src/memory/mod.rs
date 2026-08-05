// 内存管理 — 分配器 + Sv39 页表 + 地址空间
//
// 子模块：
//   allocator — 物理内存分配器（bump → hybrid → frame/block）
//   addr      — VirtAddr / PhysAddr
//   entry     — Sv39 PTE + PteFlags
//   fault     — 缺页处理
//   space     — AddressSpace、Region、内核地址空间初始化
//   table     — PageTable、页表遍历/映射

pub mod addr;
pub mod allocator;
pub mod entry;
pub mod fault;
pub mod space;
pub(crate) mod table;

/// 页表操作错误 — `AddressSpace` pub 方法返回的错误类型。
///
/// 经 `pub use` 从 `pub(crate) mod table` 导出，使 pub API 签名中的类型
/// 可通过 `crate::memory::MapError` 命名。bin crate 无外部消费者，re-export
/// 为「pub 签名类型可命名性」预留，故 allow(unused_imports)。
#[allow(unused_imports)]
pub use table::MapError;

/// 页大小 (4 KiB) — RISC-V 架构常量。
pub use crate::platform::PAGE_SIZE;
/// 页偏移位数。
pub const PAGE_SHIFT: usize = 12;
/// 任务栈固定虚拟窗口基址 — 每任务栈映射到 `[TASK_STACK_BASE, +TASK_STACK_SIZE)`。
///
/// Sv39 低半区 L2 索引 3：内核仅映射 L2 0/1/2（MMIO / PCIe / DRAM）+ 高半区，
/// L2[3] 未映射 → `from_kernel` 浅克隆后各任务克隆里该条目无效 → 每个任务映射
/// 栈时各自分配私有 L1/L0，同一 VA 互不覆盖。守护页 = `[BASE-4K, BASE)` 保持
/// 未映射（栈溢出直接触发缺页：user → terminate / kernel → panic）。
///
/// 不变量：内核不得在内核空间映射 L2[3]（0xC000_0000..0x1_0000_0000）；
/// DRAM 必须 < 1 GiB（否则与 DRAM 恒等映射重叠，`space::init` 有断言）。
pub(crate) const TASK_STACK_BASE: usize = 0xC000_0000;

/// 每个任务栈的大小（字节）。
pub(crate) const TASK_STACK_SIZE: usize = 16384;

use crate::hal::csr::satp;

/// 切换地址空间（写 satp + sfence.vma）。
///
/// `asid` 为地址空间标识符（0 = 内核，1-65535 = 用户进程）。
/// ASID 非 0 时仅刷新该 ASID 的 TLB 条目，避免全局刷新。
///
/// # Safety
///
/// 调用者需确保 `root_page_number` 指向的页表包含当前 hart 即将执行的代码映射，
/// 且 `root_page_number` 是有效的物理页号。
pub unsafe fn switch_space(root_page_number: usize, asid: usize) {
    unsafe {
        let satp_val = satp::make(satp::MODE_SV39, asid, root_page_number);
        satp::write(satp_val);
        core::arch::asm!("sfence.vma zero, {}", in(reg) asid);
    }
}

/// 刷新整个 TLB。
///
/// 发出 `sfence.vma zero, zero` 指令，使所有 hart 的 TLB 条目失效。
/// 在页表映射更改、取消映射或切换地址空间后必须调用。
///
/// # Safety
///
/// 调用者需确保刷新后页表仍然有效，且当前无其它 hart 使用即将失效的 TLB 条目。
pub unsafe fn flush_tlb() {
    unsafe {
        core::arch::asm!("sfence.vma zero, zero");
    }
}
