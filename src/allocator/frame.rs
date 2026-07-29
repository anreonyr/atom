use crate::platform::PAGE_SIZE;
use core::ptr::NonNull;

use alloc::{
    alloc::{AllocError, Allocator},
    vec::Vec,
};

use crate::{
    allocator::{bump, Link},
    info,
    lock::SpinLock,
};

struct Meta {
    free: bool,
    power: u8,
}

impl Meta {
    fn new(free: bool, power: u8) -> Self {
        Self { free, power }
    }
}

pub(crate) struct FrameAllocator {
    inner: SpinLock<Option<FrameInner>>,
}

impl FrameAllocator {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(None),
        }
    }

    /// 分两阶段初始化：先分配元数据（通过 bump），等所有 bump 分配完成后再
    /// 确定基址并构建 freelist。供 hybrid 模式使用。
    pub fn init_metadata(&self) {
        let mut guard = self.inner.lock();
        guard.replace({
            let mut inner = FrameInner::new();
            inner.init_metadata();
            inner
        });
    }

    /// 在所有 bump 分配完成后调用，确定 frame 区域基址并构建 buddy freelist。
    pub fn init_freelist(&self) {
        let mut guard = self.inner.lock();
        let inner = guard.as_mut().expect("init_metadata not called");
        inner.init_freelist();
    }

    /// 兼容旧式一步初始化（直接使用 frame 分配器时）。
    #[allow(dead_code)]
    pub fn init(&self) {
        self.init_metadata();
        self.init_freelist();
    }
}

unsafe impl Allocator for FrameAllocator {
    fn allocate(&self, layout: core::alloc::Layout) -> Result<NonNull<[u8]>, AllocError> {
        let size = layout.size().max(PAGE_SIZE);
        let power = size.next_multiple_of(PAGE_SIZE).ilog2() as usize - PAGE_SIZE.ilog2() as usize;

        let mut guard = self.inner.lock();
        let frame = guard.as_mut().ok_or(AllocError)?;

        let index = unsafe { frame.split_block(power) }.ok_or(AllocError)?;

        let addr = frame.frame_addr(index) as *mut u8;
        info!("address {:?}, frame index {}, power {}", addr, index, power);
        Ok(NonNull::slice_from_raw_parts(
            NonNull::new(addr).ok_or(AllocError)?,
            size,
        ))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: core::alloc::Layout) {
        let mut guard = self.inner.lock();
        let Some(frame) = guard.as_mut() else { return };
        let size = layout.size().max(PAGE_SIZE);
        let power = size.next_multiple_of(PAGE_SIZE).ilog2() as usize - PAGE_SIZE.ilog2() as usize;
        let addr = ptr.addr().get();
        let index = frame.frame_index(addr);

        frame.merge_block(index, power);
    }
}

struct FrameInner {
    freelist: Vec<Option<NonNull<Link>>>,
    pagemeta: Vec<Option<Meta>>,
    base: usize,
    edge: usize,
}

impl FrameInner {
    const fn new() -> Self {
        Self {
            freelist: Vec::new(),
            pagemeta: Vec::new(),
            base: 0,
            edge: 0,
        }
    }

    /// 第一阶段：分配 freelist/pagemeta Vecs（使用暂定基址计算大小）。
    fn init_metadata(&mut self) {
        let prov_base = bump::frontier().next_multiple_of(PAGE_SIZE);
        self.edge = bump::boundary();
        let max_frame = (self.edge - prov_base) / PAGE_SIZE;
        let max_power = max_frame.ilog2() as usize + 1;

        // 使用稍大的尺寸以容纳最终基址可能的偏移
        self.freelist.resize_with(max_power, || None);
        self.pagemeta.resize_with(max_frame, || None);
    }

    /// 第二阶段：在所有 bump 分配完成后确定实际基址并构建 buddy freelist。
    fn init_freelist(&mut self) {
        // 此时 bump 分配已全部完成，frontier 不再变动
        self.base = bump::frontier().next_multiple_of(PAGE_SIZE);
        let max_frame = (self.edge - self.base) / PAGE_SIZE;
        let max_power = max_frame.ilog2() as usize + 1;

        // 收缩到实际大小（最终基址 >= 暂定基址，尺寸只减不增）
        self.freelist.resize_with(max_power, || None);
        self.pagemeta.resize_with(max_frame, || None);

        let mut index = 0usize;
        let mut remaining = max_frame;
        while remaining > 0 {
            let power = (index.trailing_zeros() as usize)
                .min(remaining.ilog2() as usize)
                .min(max_power - 1);
            unsafe {
                self.push_link(index, power);
            }
            index += 1 << power;
            remaining -= 1 << power;
        }
    }

    // 物理地址 → 帧索引
    fn frame_index(&self, addr: usize) -> usize {
        (addr - self.base) / PAGE_SIZE
    }

    // 帧索引 → 物理地址
    fn frame_addr(&self, index: usize) -> usize {
        self.base + index * PAGE_SIZE
    }

    // buddy 索引：翻转 order 对应的位
    fn buddy_index(index: usize, power: usize) -> usize {
        index ^ (1 << power)
    }

    // 从 freelist[order] 头部弹出一个空闲块，标记为非空闲，返回帧索引。
    //
    // # Safety
    //
    // 调用者需确保 freelist[order] 的链表节点指向有效的已映射物理内存。
    unsafe fn pop_link(&mut self, power: usize) -> Option<usize> {
        let head = self.freelist[power]?;
        let addr = head.addr().get();
        let index = self.frame_index(addr);

        let next = head.read().next;
        self.freelist[power] = next;
        if let Some(n) = next {
            n.read().prev = None;
        }

        self.pagemeta[index] = Some(Meta::new(false, power as u8));
        Some(index)
    }

    // 将帧索引对应的块插入 freelist[order] 头部，写入侵入式 Link 节点。
    //
    // # Safety
    //
    // 调用者需确保 index 对应的物理地址有效且未被其他方式使用。
    unsafe fn push_link(&mut self, index: usize, power: usize) {
        let addr = NonNull::new_unchecked(self.frame_addr(index) as *mut Link);
        addr.write(Link::new(None, self.freelist[power]));

        if let Some(head) = self.freelist[power] {
            head.read().prev = Some(addr);
        }

        self.freelist[power] = Some(addr);
        self.pagemeta[index] = Some(Meta::new(true, power as u8));
    }

    // 从 freelist[order] 中移除帧索引对应的块（侵入式链表摘除）。
    //
    // # Safety
    //
    // 调用者需确保 index 对应的 Link 节点确实在 freelist[order] 链表中。
    unsafe fn remove_link(&mut self, index: usize, power: usize) {
        let addr = self.frame_addr(index) as *mut Link;
        let prev = (*addr).prev;
        let next = (*addr).next;

        if let Some(p) = prev {
            (*p.as_ptr()).next = next;
        } else {
            self.freelist[power] = next;
        }
        if let Some(n) = next {
            (*n.as_ptr()).prev = prev;
        }
    }

    // 从 >=order 的空闲桶中找到块，逐级拆分到目标 order，返回分配帧索引。
    //
    // # Safety
    //
    // 内部调用 pop_link / push_link，要求 freelist 链表节点指向的物理内存有效。
    unsafe fn split_block(&mut self, power: usize) -> Option<usize> {
        // 向上找到第一个有空闲块的 order
        let mut k = power;
        while k < self.freelist.len() && self.freelist[k].is_none() {
            k += 1;
        }
        if k >= self.freelist.len() {
            return None;
        }

        let index = self.pop_link(k)?;

        // 逐级拆分：每级把 buddy 推入 freelist
        while k > power {
            k -= 1;
            let buddy = Self::buddy_index(index, k);
            self.push_link(buddy, k);
        }

        Some(index)
    }

    // 将释放的帧索引推入 freelist，并逐级向上与空闲 buddy 合并。
    //
    // # Safety
    //
    // 调用者需确保 index 来自本分配器的 allocate，且未被重复释放。
    unsafe fn merge_block(&mut self, mut index: usize, mut power: usize) {
        while power < self.freelist.len() {
            let buddy = Self::buddy_index(index, power);

            if !self.pagemeta[buddy]
                .as_ref()
                .is_some_and(|m| m.free && m.power as usize == power)
            {
                break;
            }

            self.remove_link(buddy, power);
            index = index.min(buddy); // 合并后取较小的帧索引
            power += 1;
        }

        self.push_link(index, power);
    }
}

pub(crate) static FRAME_ALLOCATOR: FrameAllocator = FrameAllocator::new();

pub fn allocator() -> &'static dyn Allocator {
    &FRAME_ALLOCATOR
}

#[allow(dead_code)]
pub fn init() {
    FRAME_ALLOCATOR.init();
}

pub fn init_metadata() {
    FRAME_ALLOCATOR.init_metadata();
}

pub fn init_freelist() {
    FRAME_ALLOCATOR.init_freelist();
}
