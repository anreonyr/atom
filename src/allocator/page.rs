// 物理页帧分配器 — 为 MMU 子系统提供 4 KiB 对齐物理页帧
//
// 实现 `core::alloc::Allocator` trait。
// 仅支持 4 KiB 对齐的单页分配（layout.size() == 4096 && layout.align() == 4096）。
// 与内核堆分配器分离，因为页表帧需要物理连续性和对齐保证。
//
// 并发安全：所有操作通过 SpinLock<PageInner> 保护。
// DRAM 范围从 `platform::config()` 动态获取（通过 DTB 探测或回退值）。
//
// OpenSBI 固件占用 DRAM 起始的 2 MiB (0x8000_0000..0x8020_0000)，
// 因此帧分配器从此偏移之后开始管理。

use alloc::vec::Vec;
use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;

use crate::lock::SpinLock;
use crate::platform;

/// 页分配器内部状态 — 由 SpinLock 保护。
struct PageInner {
    bitmap: Vec<u64>,  // 每 bit 代表一个帧：1=已分配, 0=空闲
    cursor: usize,     // 下次扫描起始 word 索引
    free_count: usize, // 剩余空闲帧计数
    total_frames: usize,
    dram_base: usize,
}

impl PageInner {
    const fn new() -> Self {
        Self {
            bitmap: Vec::new(),
            cursor: 0,
            free_count: 0,
            total_frames: 0,
            dram_base: 0,
        }
    }

    /// 初始化位图：从 platform config 读取 DRAM 范围，标记内核和栈保留帧。
    unsafe fn init(&mut self) {
        let config = platform::config();

        self.dram_base = config.dram_base + config.firmware_reserve;
        let dram_size = config.dram_size.saturating_sub(config.firmware_reserve);

        self.total_frames = dram_size / platform::PAGE_SIZE;

        let words = self.total_frames.div_ceil(64);
        self.bitmap = alloc::vec![0u64; words];

        extern "C" {
            static _kernel_end: u8;
        }
        let kernel_end = &raw const _kernel_end as usize;

        let kernel_end_frame = if kernel_end > self.dram_base {
            (kernel_end - self.dram_base).div_ceil(platform::PAGE_SIZE)
        } else {
            0
        };

        let stack_reserve_frames = config.stack_reserve / platform::PAGE_SIZE;
        let stack_start_frame = self.total_frames.saturating_sub(stack_reserve_frames);

        for frame in 0..kernel_end_frame.min(self.total_frames) {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }
        for frame in stack_start_frame..self.total_frames {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }

        // 标记 bump 分配区为已用——bump 分配的元数据（frame/block 的 Vec、
        // 以及本 bitmap 自身）不能被 page 分配器二次分配，否则 MMU 页表写入
        // 会破坏分配器内部状态。
        extern "C" {
            static _bump_base: u8;
        }
        let bump_start = &raw const _bump_base as usize;
        let bump_end = crate::allocator::bump::frontier();
        if bump_start >= self.dram_base && bump_end >= bump_start {
            for frame in (bump_start - self.dram_base) / platform::PAGE_SIZE
                .. (bump_end - self.dram_base).div_ceil(platform::PAGE_SIZE)
            {
                if frame < self.total_frames {
                    self.bitmap[frame / 64] |= 1u64 << (frame % 64);
                }
            }
        }

        self.free_count = self.bitmap.iter().map(|w| w.count_zeros() as usize).sum();
    }

    fn find_free_frame(&self) -> Option<usize> {
        if self.bitmap.is_empty() {
            return None;
        }
        let words = self.bitmap.len();
        let start = self.cursor % words;
        for offset in 0..words {
            let wi = (start + offset) % words;
            let word = self.bitmap[wi];
            if word == u64::MAX {
                continue;
            }
            let bit = (!word).trailing_zeros() as usize;
            let frame = wi * 64 + bit;
            if frame < self.total_frames {
                return Some(frame);
            }
        }
        None
    }
}

/// 位图物理页帧分配器。
///
/// 实现 `Allocator` trait，仅接受 `Layout::from_size_align(4096, 4096)`。
struct PageAllocator {
    inner: SpinLock<Option<PageInner>>,
}

impl PageAllocator {
    const fn new() -> Self {
        Self {
            inner: SpinLock::new(None),
        }
    }

    pub fn init(&self) {
        let mut guard = self.inner.lock();
        guard.replace({
            let mut inner = PageInner::new();
            // SAFETY: 在启动早期单 hart 下调用，满足位图初始化的一次性要求。
            unsafe { inner.init() };
            inner
        });
    }
}

unsafe impl Allocator for PageAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() != platform::PAGE_SIZE || layout.align() != platform::PAGE_SIZE {
            return Err(AllocError);
        }

        let mut guard = self.inner.lock();
        let s = guard.as_mut().ok_or(AllocError)?;
        if s.free_count == 0 {
            return Err(AllocError);
        }

        loop {
            let frame = s.find_free_frame().ok_or(AllocError)?;

            let wi = frame / 64;
            let bit = frame % 64;
            let mask = 1u64 << bit;

            if s.bitmap[wi] & mask != 0 {
                continue;
            }
            s.bitmap[wi] |= mask;

            s.free_count -= 1;
            s.cursor = wi;

            let addr = s.dram_base + frame * platform::PAGE_SIZE;

            // SAFETY: 帧地址在 DRAM 范围内，锁保护下无并发写入。
            unsafe {
                core::ptr::write_bytes(addr as *mut u8, 0, platform::PAGE_SIZE);
            }

            let ptr = NonNull::new(addr as *mut u8).unwrap();
            return Ok(NonNull::slice_from_raw_parts(ptr, platform::PAGE_SIZE));
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, _layout: Layout) {
        let mut guard = self.inner.lock();
        let Some(inner) = guard.as_mut() else { return };
        let pa = ptr.as_ptr() as usize;

        let end = inner.dram_base + inner.total_frames * platform::PAGE_SIZE;
        if !(inner.dram_base..end).contains(&pa) {
            return;
        }

        let frame = (pa - inner.dram_base) / platform::PAGE_SIZE;
        if frame >= inner.total_frames {
            return;
        }

        let wi = frame / 64;
        let bit = frame % 64;

        inner.bitmap[wi] &= !(1u64 << bit);
        inner.free_count += 1;

        if wi < inner.cursor {
            inner.cursor = wi;
        }
    }
}

/// 全局页分配器实例。
static PAGE_ALLOCATOR: PageAllocator = PageAllocator::new();

/// 获取页分配器的 `&'static dyn Allocator` 引用。
pub fn allocator() -> &'static dyn Allocator {
    &PAGE_ALLOCATOR
}

/// 初始化全局页分配器。
///
/// 必须在内核启动早期、单 hart 下调用一次。
pub fn init() {
    PAGE_ALLOCATOR.init();
}
