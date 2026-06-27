// 物理页帧分配器 — 为 MMU 子系统提供 4 KiB 对齐物理页帧
//
// 实现 `core::alloc::Allocator` trait。
// 仅支持 4 KiB 对齐的单页分配（layout.size() == 4096 && layout.align() == 4096）。
// 与内核堆分配器分离，因为页表帧需要物理连续性和对齐保证。
//
// 并发安全：所有操作通过 SpinLock<BitmapInner> 保护。

use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;

use crate::lock::SpinLock;


/// DRAM 基址
const DRAM_BASE: usize = 0x8000_0000;
/// DRAM 大小（QEMU virt 默认 8 MiB）
const DRAM_SIZE: usize = 8 * 1024 * 1024;
/// 页帧大小
const FRAME_SIZE: usize = 4096;
/// 总帧数 = 8 MiB / 4 KiB = 2048
const TOTAL_FRAMES: usize = DRAM_SIZE / FRAME_SIZE;
/// 位图 word 数：每个 u64 管理 64 帧，2048 / 64 = 32
const BITMAP_WORDS: usize = TOTAL_FRAMES / 64;
/// 栈保留大小（从 DRAM 末尾向下，留给内核栈使用）
const STACK_RESERVE: usize = 32 * 1024;


/// 位图分配器内部状态 — 由 SpinLock 保护。
struct BitmapInner {
    bitmap: [u64; BITMAP_WORDS], // 每 bit 代表一个帧：1=已分配, 0=空闲
    cursor: usize,               // 下次扫描起始 word 索引
    free_count: usize,           // 剩余空闲帧计数
}

impl BitmapInner {
    const fn new() -> Self {
        Self {
            bitmap: [0; BITMAP_WORDS],
            cursor: 0,
            free_count: 0,
        }
    }

    unsafe fn init(&mut self) {
        extern "C" {
            static _kernel_end: u8;
        }
        let kernel_end = &raw const _kernel_end as usize;

        let kernel_end_frame = if kernel_end > DRAM_BASE {
            (kernel_end - DRAM_BASE).div_ceil(FRAME_SIZE)
        } else {
            0
        };

        let stack_reserve_frames = STACK_RESERVE / FRAME_SIZE;
        let stack_start_frame = TOTAL_FRAMES - stack_reserve_frames;

        for frame in 0..kernel_end_frame.min(TOTAL_FRAMES) {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }
        for frame in stack_start_frame..TOTAL_FRAMES {
            self.bitmap[frame / 64] |= 1u64 << (frame % 64);
        }

        self.free_count = TOTAL_FRAMES.saturating_sub(kernel_end_frame + stack_reserve_frames);
    }

    fn find_free_frame(&self) -> Option<usize> {
        let start = self.cursor % BITMAP_WORDS;
        for offset in 0..BITMAP_WORDS {
            let wi = (start + offset) % BITMAP_WORDS;
            let word = self.bitmap[wi];
            if word == u64::MAX {
                continue;
            }
            let bit = (!word).trailing_zeros() as usize;
            let frame = wi * 64 + bit;
            if frame < TOTAL_FRAMES {
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

                let addr = DRAM_BASE + frame * FRAME_SIZE;

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

            if !(DRAM_BASE..DRAM_BASE + DRAM_SIZE).contains(&pa) {
                return;
            }

            let frame = (pa - DRAM_BASE) / FRAME_SIZE;
            if frame >= TOTAL_FRAMES {
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
