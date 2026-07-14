use core::ptr::NonNull;

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

struct FrameAllocator {}

struct Inner {
    freelist: [Option<NonNull<Link>>; 11],
    pagemeta: [Meta; 4096],
}
