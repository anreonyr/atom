// 显式空闲链表分配器
//
// 使用单向链表管理空闲块，首次适配策略。分配时拆分大块，释放时归还到链表头部。
// header 紧邻用户指针之前，dealloc 通过 ptr.byte_sub(sizeof(FreeBlock)) 即可找回 header。
//
// 堆内存来自 .bss 节的一个大数组，由链接器自动放置在内核数据之后。
//
// 使用 core::sync::atomic 提供内部可变性以满足 GlobalAlloc trait 的 &self，
// 同时依靠 RISC-V "a" 扩展（目标三元组中的 "gc" 已包含）的原子指令。

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::error;

// ── 堆大小 ─────────────────────────────────────────────────
const HEAP_SIZE: usize = 64 * 1024; // 64 KiB

/// 堆内存本体 — 放在 .bss，不占二进制体积，4 KiB 对齐以便大页分配
#[repr(align(4096))]
struct HeapMem([u8; HEAP_SIZE]);

static mut HEAP: HeapMem = HeapMem([0; HEAP_SIZE]);

// ── FreeBlock ──────────────────────────────────────────────

struct FreeBlock {
    /// 块总大小（含 FreeBlock 自身）
    size: AtomicUsize,
    /// 下一个空闲块
    next: AtomicPtr<FreeBlock>,
}

// ── BlockAllocator ─────────────────────────────────────────

pub struct BlockAllocator {
    head: AtomicPtr<FreeBlock>,
}

impl BlockAllocator {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
        }
    }

    fn init(&self) {
        unsafe {
            let block = ptr::addr_of_mut!(HEAP.0).cast::<FreeBlock>();
            (*block).size = AtomicUsize::new(HEAP_SIZE);
            (*block).next = AtomicPtr::new(null_mut());
            self.head.store(block, Ordering::Relaxed);
        }
    }
}

/// 初始化全局分配器 —— 在 `main` 早期调用一次
pub fn init() {
    ALLOCATOR.init();
}

// GlobalAlloc 要求实现 Sync
unsafe impl Sync for BlockAllocator {}

unsafe impl GlobalAlloc for BlockAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let meta = core::mem::size_of::<FreeBlock>();
        let need = layout.size();
        let align = layout.align();

        let mut prev: *mut FreeBlock = null_mut();
        let mut curr = self.head.load(Ordering::Relaxed);

        while !curr.is_null() {
            let next = (*curr).next.load(Ordering::Relaxed);
            let size = (*curr).size.load(Ordering::Relaxed);

            if size >= meta + need {
                let mut cursor = curr;
                // 在当前块内滑动搜索对齐位置
                while cursor.addr() + meta + need <= curr.addr() + size {
                    let offset = cursor.byte_add(meta).align_offset(align);
                    if offset == usize::MAX {
                        break;
                    }

                    let header = cursor.byte_add(offset);
                    let used = meta + offset + need;

                    if used > size {
                        // 当前对齐位置放不下，尝试下一个对齐边界
                        cursor = cursor.byte_add(align);
                        continue;
                    }

                    let remain = size - used;

                    // ── 拆分：尾随 + 前导间隙 ────────────────────
                    let mut link = if remain > meta {
                        let tail = header.byte_add(used);
                        (*tail).size = AtomicUsize::new(remain);
                        (*tail).next.store(next, Ordering::Relaxed);
                        tail
                    } else {
                        next
                    };

                    let gap = header.addr() - curr.addr();

                    if gap > meta {
                        (*curr).size = AtomicUsize::new(gap);
                        (*curr).next.store(link, Ordering::Relaxed);
                        link = curr;
                    }

                    if prev.is_null() {
                        self.head.store(link, Ordering::Relaxed);
                    } else {
                        (*prev).next.store(link, Ordering::Relaxed);
                    }

                    (*header).size = AtomicUsize::new(used);

                    return header.byte_add(meta).cast::<u8>();
                }
            }

            prev = curr;
            curr = next;
        }

        error!(
            "allocator: out of memory (need {} bytes, align {})\n",
            need, align
        );
        null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        // header 紧邻 ptr 之前，固定偏移
        let header = ptr
            .byte_sub(core::mem::size_of::<FreeBlock>())
            .cast::<FreeBlock>();

        let head = self.head.load(Ordering::Relaxed);
        (*header).next.store(head, Ordering::Relaxed);
        self.head.store(header, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: BlockAllocator = BlockAllocator::new();
