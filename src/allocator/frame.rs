use core::{alloc::GlobalAlloc, ptr::NonNull};

use crate::lock::SpinLock;

const MAX_POWER: usize = 11;
const MAX_PAGES: usize = 4096;

struct Link {
    prev: Option<NonNull<Link>>,
    next: Option<NonNull<Link>>,
}

impl Link {
    fn new(prev: Option<NonNull<Link>>, next: Option<NonNull<Link>>) -> Self {
        Self { prev, next }
    }
}

struct Meta {
    free: bool,
    power: u8,
}

impl Meta {
    fn new(free: bool, power: u8) -> Self {
        Self { free, power }
    }
}

struct FrameAllocator {
    inner: SpinLock<FrameInner>,
}

struct FrameInner {
    freelist: [Option<NonNull<Link>>; 11],
    pagemeta: [Meta; 4096],
}

unsafe impl GlobalAlloc for FrameAllocator {
    unsafe fn alloc(&self, _layout: core::alloc::Layout) -> *mut u8 {
        todo!()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {
        todo!()
    }
}
