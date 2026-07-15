use alloc::alloc::{AllocError, Allocator};
use core::ptr::NonNull;

use crate::{lock::SpinLock, platform};

const STACK_RESERVE: usize = 32 * 1024;

struct BumpAllocator {
    inner: SpinLock<BumpInner>,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            inner: SpinLock::new(BumpInner::new(0, 0, 0)),
        }
    }
}

struct BumpInner {
    used: usize,
    base: usize,
    edge: usize,
}

impl BumpInner {
    const fn new(base: usize, edge: usize, used: usize) -> Self {
        Self { base, edge, used }
    }

    fn init(&mut self) {
        extern "C" {
            static _bump_base: usize;
        }
        let config = platform::config();
        self.base = &raw const _bump_base as usize;
        self.edge = config.dram_base + config.dram_size - STACK_RESERVE;
    }
}

unsafe impl Allocator for BumpAllocator {
    fn allocate(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, AllocError> {
        let mut inner = self.inner.lock();

        let frontier = (inner.base + inner.used) as *mut u8;
        let next = unsafe { frontier.add(frontier.align_offset(layout.align())) };

        if next.addr() + layout.size() > inner.edge {
            return Err(AllocError);
        }

        inner.used = next.addr() - inner.base + layout.size();
        Ok(NonNull::slice_from_raw_parts(
            NonNull::new(next).ok_or(AllocError)?,
            layout.size(),
        ))
    }

    unsafe fn deallocate(&self, _ptr: core::ptr::NonNull<u8>, _layout: core::alloc::Layout) {}
}

/// Bump 分配器实例 — 通过 PortalAllocator 的 trait object 间接调用。
static BUMP_ALLOCATOR: BumpAllocator = BumpAllocator::new();

/// 获取 bump 分配器的 `&'static dyn Allocator` 引用 — 供 PortalAllocator 使用。
pub fn allocator() -> &'static dyn Allocator {
    &BUMP_ALLOCATOR
}

pub fn boundary() -> usize {
    let inner = BUMP_ALLOCATOR.inner.lock();
    inner.edge
}
pub fn frontier() -> usize {
    let inner = BUMP_ALLOCATOR.inner.lock();
    inner.base + inner.used
}

/// 初始化 bump 分配器的内存区域。
///
/// # Safety
///
/// 必须在 `main` 早期调用恰好一次，在 heap 分配之前。
pub unsafe fn init() {
    BUMP_ALLOCATOR.inner.lock().init();
}
