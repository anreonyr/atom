use core::alloc::GlobalAlloc;

use crate::lock::SpinLock;

struct BumpAllocator {
    inner: SpinLock<BumpInner>,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            inner: SpinLock::new(BumpInner::new(0, 0)),
        }
    }
}

struct BumpInner {
    base: usize,
    used: usize,
}

impl BumpInner {
    const fn new(base: usize, used: usize) -> Self {
        Self { base, used }
    }
    fn init(&mut self) {
        extern "C" {
            static _kernel_end: usize;
        }
        self.base = &raw const _kernel_end as usize;
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let mut inner = self.inner.lock();
        let frontier = (inner.base + inner.used) as *mut u8;
        let aligned = frontier.add(frontier.align_offset(layout.align()));
        inner.used = aligned.sub(inner.base).addr() + layout.size();
        aligned
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

pub unsafe fn init() {
    ALLOCATOR.inner.lock().init();
}
