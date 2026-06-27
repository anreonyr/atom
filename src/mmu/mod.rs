// MMU 子系统 — Sv39 虚拟内存
//
// 使用 identity-mapping（VA==PA）方式启用 Sv39 分页。

pub mod addr;
pub mod alloc;
pub mod entry;
pub mod fault;
pub mod space;
pub mod table;

use crate::hal::csr::satp;
use crate::lock::SpinLock;

use self::addr::{PhysAddr, VirtAddr};
use self::alloc::PageFrameAllocator;
use self::entry::PteFlags;
use self::space::AddressSpace;
use self::table::PageTable;

// ── 页表常量 ────────────────────────────────────────────────────

/// 页大小 (4 KiB)
pub const PAGE_SIZE: usize = 4096;
/// 页偏移位数
pub const PAGE_SHIFT: usize = 12;

// ── 全局内核地址空间 ────────────────────────────────────────────

/// 内核地址空间。`mmu::init()` 创建并写入，此后只读访问。
pub static KERNEL_SPACE: SpinLock<Option<AddressSpace>> = SpinLock::new(None);

// ── QEMU virt 物理内存区域 ──────────────────────────────────────
// TODO: 将来移到平台配置模块

/// DRAM 基址
pub const DRAM_BASE: usize = 0x8000_0000;
/// DRAM 大小（QEMU virt 默认 8 MiB）
pub const DRAM_SIZE: usize = 8 * 1024 * 1024;

/// UART NS16550A 基址 (IRQ 10)
pub const UART_BASE: usize = 0x1000_0000;
/// UART 映射大小
pub const UART_SIZE: usize = 0x1000;

/// CLINT 基址 (mtime / mtimecmp / MSIP)
pub const CLINT_BASE: usize = 0x0200_0000;
/// CLINT 映射大小
pub const CLINT_SIZE: usize = 0x10000;

/// PLIC 中断控制器基址
pub const PLIC_BASE: usize = 0x0C00_0000;
/// PLIC 映射大小
pub const PLIC_SIZE: usize = 0x10000;

// ── 公开 API ────────────────────────────────────────────────────

/// 初始化 MMU：创建内核地址空间，identity-map DRAM 和 MMIO，启用 Sv39 分页
///
/// 必须在 `allocator::init()` 之后、在驱动程序 MMIO 访问之前调用。
///
/// # Safety
///
/// 写入 `satp` 后会立即启用分页。调用者需确保此时所有存活的指针
/// （栈、代码、数据段）都已 identity-mapped。
pub unsafe fn init(alloc: &dyn PageFrameAllocator) {
    // 1. 创建内核地址空间
    let kernel_space =
        AddressSpace::new(alloc).expect("mmu: failed to create kernel address space");

    // 2. Identity-map DRAM
    let ram_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X | PteFlags::A | PteFlags::D;

    kernel_space
        .map(
            VirtAddr::new_truncate(DRAM_BASE),
            PhysAddr::from_raw(DRAM_BASE),
            DRAM_SIZE,
            ram_flags,
            alloc,
        )
        .expect("mmu: failed to identity-map DRAM");

    // 3. Identity-map MMIO 设备（无 X 位，不可执行）
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    kernel_space
        .map(
            VirtAddr::new_truncate(UART_BASE),
            PhysAddr::from_raw(UART_BASE),
            UART_SIZE,
            dev_flags,
            alloc,
        )
        .expect("mmu: failed to map UART");
    kernel_space
        .map(
            VirtAddr::new_truncate(CLINT_BASE),
            PhysAddr::from_raw(CLINT_BASE),
            CLINT_SIZE,
            dev_flags,
            alloc,
        )
        .expect("mmu: failed to map CLINT");
    kernel_space
        .map(
            VirtAddr::new_truncate(PLIC_BASE),
            PhysAddr::from_raw(PLIC_BASE),
            PLIC_SIZE,
            dev_flags,
            alloc,
        )
        .expect("mmu: failed to map PLIC");

    // 4. 建立内核高半区映射（为 S-mode 切换做准备）
    //    VA: 0xFFFF_FF80_8000_0000 → PA: 0x8000_0000
    let kernel_va_base =
        VirtAddr::new_truncate(VirtAddr::KERNEL_BASE + DRAM_BASE);
    kernel_space
        .map(
            kernel_va_base,
            PhysAddr::from_raw(DRAM_BASE),
            DRAM_SIZE,
            ram_flags,
            alloc,
        )
        .expect("mmu: failed to map kernel high-half");
    let satp_val = satp::make(satp::MODE_SV39, 0, kernel_space.root_ppn() as usize);
    satp::write(satp_val);

    // 5. 启用 Sv39 分页
    let satp_val = satp::make(satp::MODE_SV39, 0, kernel_space.root_ppn() as usize);
    satp::write(satp_val);

    // 6. 刷新 TLB
    PageTable::sfence_all();

    // 7. 保存内核地址空间
    KERNEL_SPACE.lock(|opt| *opt = Some(kernel_space));
}

/// 动态映射 MMIO 设备区域（启动后使用）
///
/// # Safety
///
/// 调用者需确保 `base` 和 `size` 描述有效的 MMIO 区域且 4 KiB 对齐。
pub unsafe fn map_device(base: usize, size: usize, alloc: &dyn PageFrameAllocator) {
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    KERNEL_SPACE.lock(|opt| {
        if let Some(ref ks) = *opt {
            ks.map(
                VirtAddr::new_truncate(base),
                PhysAddr::from_raw(base),
                size,
                dev_flags,
                alloc,
            )
            .expect("mmu: map_device failed");
            PageTable::sfence_all();
        }
    });
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
    PageTable::sfence_all();
}

/// 刷新整个 TLB。
pub unsafe fn flush_tlb() {
    PageTable::sfence_all();
}

/// 创建一个新的用户地址空间（内核半区共享，用户半区为空）。
pub fn new_user_space(alloc: &dyn PageFrameAllocator) -> Result<AddressSpace, table::MapError> {
    let mut space = AddressSpace::new(alloc)?;
    KERNEL_SPACE.lock(|opt| {
        if let Some(ref ks) = *opt {
            space.share_kernel_half(ks);
        }
    });
    Ok(space)
}
