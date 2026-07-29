// 混合路由分配器 — 按大小委派给 block 或 frame 后端
//
// layout.size() <= PAGE_SIZE  → block::allocator()（segregated free list）
// layout.size() >  PAGE_SIZE  → frame::allocator()（buddy）
//
// hybrid 自身不管理任何内存，仅检查大小并路由。block 内部缺页时
// 直接调用 frame::allocator() 取页（锁序：block→frame，从不反向）。

use core::alloc::Layout;
use core::ptr::NonNull;

use alloc::alloc::{AllocError, Allocator};

use crate::allocator::{block, frame};
use crate::platform::PAGE_SIZE;

pub(crate) struct HybridAllocator;

impl HybridAllocator {
    pub const fn new() -> Self {
        Self
    }

    /// 初始化 block + frame 后端。
    ///
    /// 分三阶段：
    ///   1. frame::init_metadata() — 分配 buddy 元数据（经 bump）
    ///   2. block::init() — 分配 block 元数据（经 bump）
    ///   3. frame::init_freelist() — 在所有 bump 分配完成后确定 frame 基址并构建 freelist
    ///
    /// 阶段 1→2→3 的顺序确保 frame 的 Link 节点不被后续 bump 分配覆盖。
    pub fn init(&self) {
        frame::init_metadata();
        block::init();
        frame::init_freelist();
    }
}

unsafe impl Allocator for HybridAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() <= PAGE_SIZE {
            block::allocator().allocate(layout)
        } else {
            frame::allocator().allocate(layout)
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() <= PAGE_SIZE {
            block::allocator().deallocate(ptr, layout);
        } else {
            frame::allocator().deallocate(ptr, layout);
        }
    }
}

pub(crate) static HYBRID_ALLOCATOR: HybridAllocator = HybridAllocator::new();

pub fn allocator() -> &'static dyn Allocator {
    &HYBRID_ALLOCATOR
}

pub fn init() {
    HYBRID_ALLOCATOR.init();
}
