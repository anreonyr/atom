// HybridAllocator — #[global_allocator] 入口，在 Buddy 和 Slab 间分发
//
// # 分配路径
//
// | 请求大小     | 对齐      | 分配器      |
// |-------------|-----------|------------|
// | ≤ 512       | ≤ 8       | Slab 缓存   |
// | ≤ 512       | > 8       | Buddy       |
// | > 512       | 任意      | Buddy       |
//
// 并发安全：Buddy/Slab 的内部状态由各自的 SpinLock 保护。
// HybridAllocator 自身无状态，仅在 alloc/dealloc 时获取对应锁。

use core::alloc::{GlobalAlloc, Layout};

use super::{buddy, slab};

/// 混合分配器 — 对外暴露为 #[global_allocator]。
pub struct HybridAllocator;

// 所有可变状态由 SpinLock 保护 — 内部可变性安全
unsafe impl Sync for HybridAllocator {}

unsafe impl GlobalAlloc for HybridAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // 零大小分配：返回固定非空对齐指针（Rust 标准行为）
        if size == 0 {
            return align as *mut u8;
        }

        // ── Slab 快速路径 ──
        if let Some(ci) = slab::cache_index(size, align) {
            let ptr = slab::SLAB.lock(|state| state.alloc(ci));
            if !ptr.is_null() {
                return ptr;
            }
            // slab OOM → 回退到 buddy（系统可能还有碎片内存）
        }

        // ── Buddy 路径 ──
        buddy::BUDDY.lock(|state| state.alloc_sized(size, align))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || layout.size() == 0 {
            return;
        }

        let size = layout.size();
        let align = layout.align();

        if let Some(ci) = slab::cache_index(size, align) {
            slab::SLAB.lock(|state| state.free(ptr, ci));
        } else {
            buddy::BUDDY.lock(|state| state.free(ptr));
        }
    }
}


#[global_allocator]
static ALLOCATOR: HybridAllocator = HybridAllocator;
