// 块分配器 — segregated free list，单链表侵入式
//
// 将大块内存划分为 2^power 大小的 block，每块前 8 字节用 `Option<NonNull<u8>>`
// 存下一块的指针（利用 niche optimization: None = 0, Some = 指针值）。
// freepool[power] 指向该 size class 的空闲链表头部。
//
// 内存从 frame allocator（buddy）获取，按需懒分配新页。
// 每页单独追踪引用计数，全部 block 释放后整页归还。
// block 大小范围：2^3 .. 2^12（8 字节 .. 4096 字节 = PAGE_SIZE）。
// 最小对齐 8 字节，申请量不足 8 字节时自动向上取整。

use core::alloc::Layout;
use core::ptr::NonNull;

use alloc::alloc::Allocator;
use alloc::vec::Vec;

use crate::allocator::{frame::allocator, PAGE_SIZE};
use crate::lock::SpinLock;

const MIN_POWER: usize = 3;
const MAX_POWER: usize = PAGE_SIZE.ilog2() as usize;

struct BlockAllocator {
    inner: SpinLock<Option<BlockInner>>,
}

impl BlockAllocator {
    const fn new() -> Self {
        Self {
            inner: SpinLock::new(None),
        }
    }

    pub fn init(&self) {
        let mut guard = self.inner.lock();
        guard.replace({
            let mut inner = BlockInner::new();
            inner.init();
            inner
        });
    }
}

unsafe impl Allocator for BlockAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
        let power = block_power(layout);
        let block_size = 1usize << power;

        if layout.align() > block_size {
            return Err(alloc::alloc::AllocError);
        }

        let mut guard = self.inner.lock();
        let inner = guard.as_mut().ok_or(alloc::alloc::AllocError)?;

        // 从 freelist 头部弹出
        if let Some(head) = inner.freepool[power] {
            let next = unsafe { head.cast::<Option<NonNull<u8>>>().read() };
            inner.freepool[power] = next;
            inner.increase_used(head, power);
            return Ok(NonNull::slice_from_raw_parts(head, block_size));
        }

        unsafe { inner.refill(power) }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let power = block_power(layout);
        let mut guard = self.inner.lock();
        let Some(inner) = guard.as_mut() else { return };

        // 头插：将 freed block 写入 freelist 头部
        ptr.cast::<Option<NonNull<u8>>>()
            .write(inner.freepool[power]);
        inner.freepool[power] = Some(ptr);

        // 该 pool 在用数 -1，归零时整页归还
        inner.decrease_used(ptr, power);
    }
}

struct Meta {
    base: usize,
    used: usize,
}

struct BlockInner {
    freepool: Vec<Option<NonNull<u8>>>,
    poolmeta: Vec<Vec<Meta>>,
}

impl BlockInner {
    fn new() -> Self {
        Self {
            freepool: Vec::new(),
            poolmeta: Vec::new(),
        }
    }

    fn init(&mut self) {
        self.freepool.resize_with(MAX_POWER + 1, || None);
        self.poolmeta.resize_with(MAX_POWER + 1, Vec::new);
    }

    /// 根据 block 地址找到所属 pool，在用数 +1。
    fn increase_used(&mut self, block: NonNull<u8>, power: usize) {
        let base = block.as_ptr() as usize & !(PAGE_SIZE - 1);
        if let Some(m) = self.poolmeta[power].iter_mut().find(|m| m.base == base) {
            m.used += 1;
        }
    }

    /// 根据 pool 基址，在用数 -1。归零时整页归还。
    fn decrease_used(&mut self, block: NonNull<u8>, power: usize) {
        let base = block.as_ptr() as usize & !(PAGE_SIZE - 1);
        let meta = match self.poolmeta[power].iter_mut().find(|m| m.base == base) {
            Some(m) => m,
            None => return,
        };
        meta.used = meta.used.saturating_sub(1);
        if meta.used > 0 {
            return;
        }

        self.freepool[power] = unsafe { purge_freelist(self.freepool[power], base) };

        // 归还给 frame allocator
        unsafe {
            allocator().deallocate(
                NonNull::new_unchecked(base as *mut u8),
                Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap(),
            )
        };

        // 移除追踪记录
        self.poolmeta[power].retain(|m| m.base != base);
    }

    unsafe fn refill(&mut self, power: usize) -> Result<NonNull<[u8]>, alloc::alloc::AllocError> {
        let block_size = 1usize << power;
        let block_nums = PAGE_SIZE / block_size;

        let page = allocator()
            .allocate(Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap())
            .map_err(|_| alloc::alloc::AllocError)?;

        let base = page.cast::<u8>().as_ptr() as usize;

        link_blocks(base, block_nums, block_size);

        // 弹出第一个 block 返回，其余留在 freelist
        let first = NonNull::new_unchecked(base as *mut u8);
        self.freepool[power] = first.cast::<Option<NonNull<u8>>>().read();

        // 追踪该 pool：初始 used = 1（即将返回的 first block）
        self.poolmeta[power].push(Meta { base, used: 1 });

        Ok(NonNull::slice_from_raw_parts(first, block_size))
    }
}

fn block_power(layout: Layout) -> usize {
    let size = layout.size().max(1usize << MIN_POWER);
    let power = size.next_power_of_two().ilog2() as usize;
    power.clamp(MIN_POWER, MAX_POWER)
}

/// 将 `block_nums` 个等大连续 block 串成单向链表。
unsafe fn link_blocks(base: usize, block_nums: usize, block_size: usize) {
    for i in 0..block_nums.saturating_sub(1) {
        let this = base + i * block_size;
        let next = base + (i + 1) * block_size;
        NonNull::new_unchecked(this as *mut Option<NonNull<u8>>)
            .write(Some(NonNull::new_unchecked(next as *mut u8)));
    }
    if block_nums > 0 {
        NonNull::new_unchecked((base + (block_nums - 1) * block_size) as *mut Option<NonNull<u8>>)
            .write(None);
    }
}

/// 遍历 freepool，移除属于指定 pool（页）的所有 block 条目。
unsafe fn purge_freelist(head: Option<NonNull<u8>>, pool_base: usize) -> Option<NonNull<u8>> {
    let pool_end = pool_base + PAGE_SIZE;
    let mut new_head = None;
    let mut last: Option<NonNull<u8>> = None;
    let mut this = head;

    while let Some(node) = this {
        let addr = node.as_ptr() as usize;
        let next: Option<NonNull<u8>> = node.cast::<Option<NonNull<u8>>>().read();

        if !(addr >= pool_base && addr < pool_end) {
            if new_head.is_none() {
                new_head = Some(node);
            }
            if let Some(p) = last {
                p.cast::<Option<NonNull<u8>>>().write(Some(node));
            }
            last = Some(node);
        }

        this = next;
    }

    // 尾 block 的 next 置 None
    if let Some(p) = last {
        p.cast::<Option<NonNull<u8>>>().write(None);
    }

    new_head
}
