// 物理页帧分配器 — 为 MMU 子系统提供 4 KiB 对齐物理页帧
//
// 实现 `core::alloc::Allocator` trait。
// 仅支持 4 KiB 对齐的单页分配（layout.size() == 4096 && layout.align() == 4096）。
// 与内核堆分配器分离，因为页表帧需要物理连续性和对齐保证。
//
// 并发安全：所有操作通过 SpinLock<BitmapInner> 保护。
// DRAM 范围从 `platform::config()` 动态获取（通过 DTB 探测或回退值）。
//
// OpenSBI 固件占用 DRAM 起始的 2 MiB (0x8000_0000..0x8020_0000)，
// 因此帧分配器从此偏移之后开始管理。

use alloc::vec::Vec; // CLEANUP(allocator): 位图改为静态数组（DRAM 大小在编译/启动时已知）
use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;

use crate::lock::SpinLock;
use crate::platform;

/// OpenSBI 固件保留大小 (2 MiB)。
const FIRMWARE_RESERVE: usize = 2 * 1024 * 1024;
/// 页帧大小
const FRAME_SIZE: usize = 4096;
/// 栈保留大小（从可用内存末尾向下，留给内核栈使用）
const STACK_RESERVE: usize = 32 * 1024;

/// 位图分配器内部状态 — 由 SpinLock 保护。
struct BitmapInner {
    bitmap: Vec<u64>,   // 每 bit 代表一个帧：1=已分配, 0=空闲
    cursor: usize,      // 下次扫描起始 word 索引
    free_count: usize,  // 剩余空闲帧计数
    total_frames: usize,
    dram_base: usize,
}

impl BitmapInner {
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
        let cfg = platform::config();

        self.dram_base = cfg.dram_base + FIRMWARE_RESERVE;
        let dram_size = cfg.dram_size.saturating_sub(FIRMWARE_RESERVE);

        self.total_frames = dram_size / FRAME_SIZE;

        let words = self.total_frames.div_ceil(64);
        self.bitmap = alloc::vec![0u64; words];

        extern "C" {
            static _kernel_end: u8;
        }
        let kernel_end = &raw const _kernel_end as usize;

        let kernel_end_frame = if kernel_end > self.dram_base {
            (kernel_end - self.dram_base).div_ceil(FRAME_SIZE)
        } else {
            0
        };

        let stack_reserve_frames = STACK_RESERVE / FRAME_SIZE;
        let stack_start_frame = self.total_frames.saturating_sub(stack_reserve_frames);

        for frame in 0..kernel_end_frame.min(self.total_frames) {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }
        for frame in stack_start_frame..self.total_frames {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }

        self.free_count = self
            .total_frames
            .saturating_sub(kernel_end_frame + stack_reserve_frames);
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
pub struct BitmapAllocator {
    inner: SpinLock<BitmapInner>,
}

impl BitmapAllocator {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(BitmapInner::new()),
        }
    }
}

unsafe impl Allocator for BitmapAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() != FRAME_SIZE || layout.align() != FRAME_SIZE {
            return Err(AllocError);
        }

        self.inner.lock(|s| {
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

                let addr = s.dram_base + frame * FRAME_SIZE;

                // SAFETY: 帧地址在 DRAM 范围内，锁保护下无并发写入。
                unsafe {
                    core::ptr::write_bytes(addr as *mut u8, 0, FRAME_SIZE);
                }

                let ptr = NonNull::new(addr as *mut u8).unwrap();
                return Ok(NonNull::slice_from_raw_parts(ptr, FRAME_SIZE));
            }
        })
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, _layout: Layout) {
        self.inner.lock(|s| {
            let pa = ptr.as_ptr() as usize;

            let end = s.dram_base + s.total_frames * FRAME_SIZE;
            if !(s.dram_base..end).contains(&pa) {
                return;
            }

            let frame = (pa - s.dram_base) / FRAME_SIZE;
            if frame >= s.total_frames {
                return;
            }

            let wi = frame / 64;
            let bit = frame % 64;

            s.bitmap[wi] &= !(1u64 << bit);
            s.free_count += 1;

            if wi < s.cursor {
                s.cursor = wi;
            }
        });
    }
}

/// 全局物理帧分配器实例。
pub static FRAME_ALLOCATOR: BitmapAllocator = BitmapAllocator::new();

/// 初始化全局物理帧分配器。
///
/// # Safety
///
/// 必须在内核启动早期、单 hart 下调用一次。
pub unsafe fn init() {
    FRAME_ALLOCATOR.inner.lock(|state| state.init());
}
