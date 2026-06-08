// 简易 Bump Allocator
//
// 只在分配时向前推进指针，不释放内存 — 适合内核启动后一次性分配的场景。
// 堆内存来自 .bss 节的一个大数组，由链接器自动放置在内核数据之后。
//
// 使用 core::sync::atomic 提供内部可变性以满足 GlobalAlloc trait 的 &self，
// 同时依靠 RISC-V "a" 扩展（目标三元组中的 "gc" 已包含）的原子指令。

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

// ── 堆大小 ─────────────────────────────────────────────────
const HEAP_SIZE: usize = 64 * 1024; // 64 KiB

/// 堆内存本体 — 放在 .bss，不占二进制体积，4 KiB 对齐以便大页分配
#[repr(align(4096))]
struct HeapMem([u8; HEAP_SIZE]);

static mut HEAP: HeapMem = HeapMem([0; HEAP_SIZE]);

// ── BumpAllocator ──────────────────────────────────────────

pub struct BumpAllocator {
    /// 下一个可用字节的地址
    next: AtomicUsize,
    /// 堆尾地址（开区间）
    end: AtomicUsize,
}

impl BumpAllocator {
    /// 创建未初始化的实例（指针均为 0，调用 `init` 前不可分配）
    const fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
        }
    }

    /// 绑定堆内存区域
    fn init(&self) {
        let start = unsafe { core::ptr::addr_of!(HEAP.0) } as usize;
        self.next.store(start, Ordering::Relaxed);
        self.end.store(start + HEAP_SIZE, Ordering::Relaxed);
    }
}

/// 初始化全局分配器 —— 在 `rust_main` 早期调用一次
pub fn init() {
    ALLOCATOR.init();
}

// GlobalAlloc 要求实现 Sync
unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let next = self.next.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);

        // 按 layout.align() 对齐
        let align = layout.align();
        let aligned = (next + align - 1) & !(align - 1);

        let new_next = aligned + layout.size();
        if new_next > end {
            // 堆耗尽
            return core::ptr::null_mut();
        }

        self.next.store(new_next, Ordering::Relaxed);
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // bump allocator 不回收内存
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();
