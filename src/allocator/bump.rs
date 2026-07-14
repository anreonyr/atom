use core::alloc::GlobalAlloc;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::lock::SpinLock;

struct BumpAllocator {
    /// 基地址，首次 alloc 时从 `_kernel_end` 符号初始化。
    base: AtomicUsize,
    inner: SpinLock<BumpInner>,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            base: AtomicUsize::new(0),
            inner: SpinLock::new(BumpInner::new(0)),
        }
    }

    /// 返回 bump 区域起始地址，首次调用时惰性初始化。
    fn base(&self) -> usize {
        let b = self.base.load(Ordering::Relaxed);
        if b != 0 {
            return b;
        }
        extern "C" {
            static _kernel_end: usize;
        }
        let actual = &raw const _kernel_end as usize;
        self.base.store(actual, Ordering::Relaxed);
        actual
    }
}

struct BumpInner {
    used: usize,
}

impl BumpInner {
    const fn new(used: usize) -> Self {
        Self { used }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.inner.lock(|inner| {
            let base = self.base();
            let r = (base + inner.used) as *mut u8;
            let t = r.add(r.align_offset(layout.align()));
            inner.used = t.sub(base).addr() + layout.size();
            return t;
        })
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();
