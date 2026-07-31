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

/// 页大小 (4 KiB) — RISC-V 架构常量。
pub use crate::platform::PAGE_SIZE;
/// 页偏移位数。
pub const PAGE_SHIFT: usize = 12;

use core::alloc::Allocator;

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
/// # Safety
///
/// 调用者需确保 `base` 和 `size` 描述有效的 MMIO 区域且 4 KiB 对齐。
pub unsafe fn map_device(base: usize, size: usize, allocator: &dyn Allocator) {
    use crate::memory::{
        addr::{PhysAddr, VirtAddr},
        entry::PteFlags,
        space::kernel_space,
    };

    let dev_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D | PteFlags::G;

    let guard = kernel_space();
    if let Some(ref ks) = *guard {
        ks.map(
            VirtAddr::new_truncate(base),
            PhysAddr::from_raw(base),
            size,
            dev_flags,
            allocator,
        )
        .expect("memory: map_device failed");
        flush_tlb();
    }
}
