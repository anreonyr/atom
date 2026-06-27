// 页表模块 — Sv39 虚拟内存子系统
//
// 使用 identity-mapping（VA==PA）方式启用 Sv39 分页。
// 页表节点使用专用静态池分配（绕过全局分配器，避免对齐问题）。

pub mod pt;
pub mod pte;

use core::ops::Index;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::hal::csr::satp;
use crate::lock::SpinLock;

use self::pt::{PageTable, PAGE_SHIFT};
use self::pte::PteFlags;

/// 全局根页表指针（Level 2 顶层页表）
pub static ROOT_PAGE_TABLE: SpinLock<Option<*mut PageTable>> = SpinLock::new(None);

// ── 页表节点专用静态池 ──────────────────────────────────
//
// 使用专用池而非全局分配器，因为全局分配器在处理 4 KiB 大对齐
// 请求时可能返回非页对齐地址。静态池保证每个 PageTable 自然对齐。

const POOL_CAP: usize = 16;

#[repr(align(4096))]
struct PageTablePool {
    pages: [PageTable; POOL_CAP],
}

static mut POOL: PageTablePool = PageTablePool {
    pages: [PageTable::new(); POOL_CAP],
};

static POOL_NEXT: AtomicUsize = AtomicUsize::new(0);

/// 从静态池分配一个页表节点（零初始化）
pub(crate) fn alloc_table() -> *mut PageTable {
    let i = POOL_NEXT.fetch_add(1, Ordering::Acquire);
    if i >= POOL_CAP {
        panic!("mmu: page table pool exhausted");
    }
    let ptr = ptr::from_mut(unsafe { &mut POOL.pages[i] });
    unsafe { core::ptr::write_bytes(ptr, 0u8, 1) };
    ptr
}

// ── QEMU virt 物理内存区域 ────────────────────────────────

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

/// 初始化 MMU：创建根页表，identity-map DRAM 和 MMIO，启用 Sv39 分页
///
/// 必须在 `allocator::init()` 之后、在驱动程序 MMIO 访问之前调用。
///
/// # Safety
///
/// 写入 `satp` 后会立即启用分页。调用者需确保此时所有存活的指针
/// （栈、代码、数据段）都已 identity-mapped。
pub unsafe fn init() {
    // 1. 分配根页表
    let root = alloc_table();

    // 2. Identity-map DRAM
    let ram_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X | PteFlags::A | PteFlags::D;

    unsafe { PageTable::map_region(root, DRAM_BASE, DRAM_BASE, DRAM_SIZE, ram_flags) };

    // 3. Identity-map MMIO 设备（无 X 位，不可执行）
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    PageTable::map_region(root, UART_BASE, UART_BASE, UART_SIZE, dev_flags);
    PageTable::map_region(root, CLINT_BASE, CLINT_BASE, CLINT_SIZE, dev_flags);
    PageTable::map_region(root, PLIC_BASE, PLIC_BASE, PLIC_SIZE, dev_flags);

    // 4. 启用 Sv39 分页
    let root_ppn = (root as usize) >> PAGE_SHIFT;
    let satp_val = satp::make(satp::MODE_SV39, 0, root_ppn);
    satp::write(satp_val);

    // 5. 刷新 TLB
    PageTable::sfence();

    // 6. 保存根页表指针
    ROOT_PAGE_TABLE.lock(|opt| *opt = Some(root));
}

/// 动态映射 MMIO 设备区域（启动后使用）
///
/// # Safety
///
/// 调用者需确保 `base` 和 `size` 描述有效的 MMIO 区域且 4 KiB 对齐。
pub unsafe fn map_device(base: usize, size: usize) {
    let dev_flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;

    ROOT_PAGE_TABLE.lock(|opt| {
        if let Some(root) = *opt {
            PageTable::map_region(root, base, base, size, dev_flags);
            PageTable::sfence();
        }
    });
}
