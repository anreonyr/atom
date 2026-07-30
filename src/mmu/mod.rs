// MMU 子系统 — Sv39 虚拟内存
//
// 使用 identity-mapping（VA == PA）方式启用 Sv39 分页。
// 内存映射区域从 `platform::config()` 动态获取。

pub mod addr;
pub mod entry;
pub mod fault;
pub mod space;
pub mod table;

use core::alloc::Allocator;

use crate::hal::csr::satp;
use crate::lock::RelLock;
use crate::platform;

use self::addr::{PhysAddr, VirtAddr};
use self::entry::PteFlags;
use self::space::AddressSpace;

/// 页大小 (4 KiB) — RISC-V 架构常量，所有 Sv 分页模式通用。
pub use crate::platform::PAGE_SIZE;
/// 页偏移位数。
pub const PAGE_SHIFT: usize = 12;

/// 内核地址空间。`mmu::init()` 创建并写入，此后只读访问。
///
/// 用 RelLock（可重入锁）：持有此锁期间若触发缺页，缺页处理器（trap.rs）
/// 会在同一 hart 上再次获取它——RelLock 允许同 hart 重入，避免自旋死锁；
/// 不同 hart 之间仍互斥。
pub static KERNEL_SPACE: RelLock<Option<AddressSpace>> = RelLock::new(None);

/// 初始化 MMU：创建内核地址空间，identity-map DRAM 和 MMIO，启用 Sv39 分页。
///
/// 必须在 `allocator::init()` 之后、在驱动程序 MMIO 访问之前调用。
///
/// # Safety
///
/// 写入 `satp` 后会立即启用分页。调用者需确保此时所有存活的指针
/// （栈、代码、数据段）都已 identity-mapped。
pub unsafe fn init() {
    let alloc = crate::allocator::page::allocator();
    let cfg = platform::config();

    // 1. 创建内核地址空间
    let kernel_space =
        AddressSpace::new(alloc).expect("mmu: failed to create kernel address space");

    // 2. Identity-map DRAM
    let ram_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X | PteFlags::A | PteFlags::D;

    kernel_space
        .map(
            VirtAddr::new_truncate(cfg.dram_base),
            PhysAddr::from_raw(cfg.dram_base),
            cfg.dram_size,
            ram_flags,
            alloc,
        )
        .expect("mmu: failed to identity-map DRAM");

    // 3. Identity-map MMIO 设备（无 X 位，不可执行）
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    crate::drivers::for_each(|dev| {
        let size = if dev.size > 0 { dev.size } else { 0x1000 };
        kernel_space
            .map(
                VirtAddr::new_truncate(dev.base),
                PhysAddr::from_raw(dev.base),
                size,
                dev_flags,
                alloc,
            )
            .expect("mmu: failed to map MMIO device");
    });

    // 4. 建立内核高半区映射（为 S-mode 切换做准备）
    //    VA: KERNEL_BASE + dram_base → PA: dram_base
    let kernel_va_base = VirtAddr::new_truncate(VirtAddr::KERNEL_BASE + cfg.dram_base);
    kernel_space
        .map(
            kernel_va_base,
            PhysAddr::from_raw(cfg.dram_base),
            cfg.dram_size,
            ram_flags,
            alloc,
        )
        .expect("mmu: failed to map kernel high-half");

    // 5. 启用 Sv39 分页
    let satp_val = satp::make(satp::MODE_SV39, 0, kernel_space.root_ppn() as usize);
    satp::write(satp_val);

    // 6. 刷新 TLB
    flush_tlb();

    // 7. 保存内核地址空间
    *KERNEL_SPACE.lock() = Some(kernel_space);
}

/// 动态映射 MMIO 设备区域（启动后使用）。
///
/// # Safety
///
/// 调用者需确保 `base` 和 `size` 描述有效的 MMIO 区域且 4 KiB 对齐。
pub unsafe fn map_device(base: usize, size: usize, alloc: &dyn Allocator) {
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    let guard = KERNEL_SPACE.lock();
    if let Some(ref ks) = *guard {
        ks.map(
            VirtAddr::new_truncate(base),
            PhysAddr::from_raw(base),
            size,
            dev_flags,
            alloc,
        )
        .expect("mmu: map_device failed");
        flush_tlb();
    }
}

/// 切换活动地址空间（写 satp + sfence.vma）。
///
/// 由调度器在上下文切换时调用。
///
/// # Safety
///
/// 调用者需确保 `space` 的页表包含当前 hart 即将执行的代码映射。
pub unsafe fn switch_space(space: &AddressSpace) {
    let satp_val = satp::make(satp::MODE_SV39, 0, space.root_ppn() as usize);
    satp::write(satp_val);
    flush_tlb();
}

/// 刷新整个 TLB。
///
/// # Safety
///
/// 调用者需确保刷新后页表仍然有效。
pub unsafe fn flush_tlb() {
    core::arch::asm!("sfence.vma zero, zero");
}

/// 创建一个新的用户地址空间（内核半区共享，用户半区为空）。
///
/// # Errors
///
/// 根页表分配失败时返回 [`MapError::OutOfMemory`](table::MapError)。
pub fn new_user_space(alloc: &dyn Allocator) -> Result<AddressSpace, table::MapError> {
    let mut space = AddressSpace::new(alloc)?;
    let guard = KERNEL_SPACE.lock();
    if let Some(ref ks) = *guard {
        space.share_kernel_half(ks);
    }
    Ok(space)
}
