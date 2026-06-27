// 页帧分配器 — MMU 子系统对物理内存分配的需求接口
//
// 与内核堆分配器（allocator.rs）分离，因为：
// 1. 页表需要 4 KiB 对齐的物理页，碎片化后的堆无法保证
// 2. 页表帧应从预留池分配，不与内核堆竞争
// 3. 不同平台可用不同分配器（buddy、bitmap）

use core::mem::size_of;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mmu::addr::PhysAddr;
use crate::mmu::table::PageTable;

// ── PageFrameAllocator trait ─────────────────────────────────────

/// 物理页帧分配器接口。
///
/// MMU 子系统通过此 trait 获取/释放存放页表节点的 4 KiB 物理页帧。
/// 与内核堆分配器（`GlobalAlloc`）分离，由物理内存管理器独立实现。
pub trait PageFrameAllocator {
    /// 分配一个零初始化的 4 KiB 物理页帧。
    ///
    /// 返回 None 表示物理内存耗尽。
    fn alloc_frame(&self) -> Option<PhysAddr>;

    /// 释放之前通过 `alloc_frame` 分配的物理页帧。
    fn free_frame(&self, _addr: PhysAddr) {
        // 默认不释放 — bootstrap 分配器和简单分配器可跳过
    }
}

// ── BootstrapPageAllocator ───────────────────────────────────────

/// 启动阶段页帧分配器。
///
/// 使用 16 页 × 4 KiB 静态池，仅在 `mmu::init()` 期间使用。
/// 后续替换为真正的物理内存分配器（buddy / bitmap）。
pub struct BootstrapPageAllocator;

const POOL_CAP: usize = 16;

/// 静态页表池 + 分配游标，绑在一起避免多个 `static mut`。
#[repr(align(4096))]
struct Pool {
    pages: [PageTable; POOL_CAP],
    next: AtomicUsize,
}

static POOL: Pool = Pool {
    pages: [const { PageTable::new() }; POOL_CAP],
    next: AtomicUsize::new(0),
};

impl PageFrameAllocator for BootstrapPageAllocator {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let i = POOL.next.fetch_add(1, Ordering::Acquire);
        if i >= POOL_CAP {
            return None;
        }
        // SAFETY: 单 hart，初始化期间无并发，每个索引只分配一次
        let page = &raw const POOL.pages[i];
        // 零初始化整页
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, size_of::<PageTable>()) };
        Some(PhysAddr::from_raw(page as usize))
    }

    fn free_frame(&self, _addr: PhysAddr) {
        // Bootstrap 分配器不回收
    }
}
