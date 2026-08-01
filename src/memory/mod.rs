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
/// 可通过 `crate::memory::MapError` 命名。
pub use table::MapError;

/// 页大小 (4 KiB) — RISC-V 架构常量。
pub use crate::platform::PAGE_SIZE;
/// 页偏移位数。
pub const PAGE_SHIFT: usize = 12;

use core::alloc::Allocator;

use crate::hal::csr::satp;
use crate::memory::addr::PhysAddr;

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
    let satp_val = satp::make(satp::MODE_SV39, asid, root_page_number);
    satp::write(satp_val);
    core::arch::asm!("sfence.vma zero, {}", in(reg) asid);
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
    core::arch::asm!("sfence.vma zero, zero");
}

/// 动态映射 MMIO 设备区域（启动后使用）。
///
/// 与 `Device.base: PhysAddr` 对齐，`base` 直接收强类型物理地址，
/// 内部不再做 `usize → PhysAddr` 往返转换。identity-mapping 下
/// 虚拟地址取同值（`VirtAddr::from_raw(base.as_usize())`）。
///
/// # Safety
///
/// 调用者需确保 `base` 和 `size` 描述有效的 MMIO 区域且 4 KiB 对齐。
///
/// # Errors
///
/// - [`MapError::NotMapped`] — 内核地址空间尚未初始化。
/// - 其他错误由 [`AddressSpace::map`] 产生（对齐、已映射、内存不足）。
pub unsafe fn map_device(
    base: PhysAddr,
    size: usize,
    allocator: &dyn Allocator,
) -> Result<(), MapError> {
    use crate::memory::{addr::VirtAddr, entry::PteFlags, space::kernel_space};

    let dev_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D | PteFlags::G;

    let guard = kernel_space();
    let ks = guard.as_ref().ok_or(MapError::NotMapped)?;

    // map_region 自动将 size 向上取整到 PAGE_SIZE，兼容 DTB reg size 非对齐的场景，
    // 且内部已 flush_tlb。
    ks.map_region(
        VirtAddr::from_raw(base.as_usize()),
        base,
        size,
        dev_flags,
        allocator,
    )
}
