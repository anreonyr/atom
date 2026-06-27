// Buddy 幂次分配器 — 以 2 的幂次管理 64 KiB 内核堆
//
// 13 个阶 (order 4..=16)，对应 16 字节到 64 KiB。每阶维护一条空闲链表。
// 分配时从请求阶向上搜索，找到后逐级拆分；释放时自动合并相邻兄弟块。
//
// 并发安全：所有操作通过 SpinLock<BuddyState> 保护。
// 核心操作封装为 BuddyState 的方法，由调用者在持有锁后调用。
//
// Buddy 地址计算：
//   buddy = (addr - heap_base) ^ (1 << order) + heap_base
// 相对堆基址做 XOR，正确处理堆不在地址 0 的情况（DRAM 在 0x8000_0000）。
//
// 堆内存来自 .bss 节的 64 KiB 数组，由本模块自行管理。

use core::ptr;

use crate::lock::SpinLock;

const HEAP_SIZE: usize = 64 * 1024; // 64 KiB

/// 堆内存本体 — 放在 .bss，不占二进制体积，4 KiB 对齐
#[repr(align(4096))]
struct HeapMem([u8; HEAP_SIZE]);

static mut HEAP: HeapMem = HeapMem([0; HEAP_SIZE]);

/// Buddy 块头部大小
pub const BUDDY_HEADER_SIZE: usize = 8;

/// 最小阶：2^4 = 16 字节（头部 8 字节 + 最小有效载荷 8 字节）
pub const MIN_ORDER: u8 = 4;

/// 最大阶：2^16 = 64 KiB（整个堆）
pub const MAX_ORDER: u8 = 16;

/// 阶数总数：16 - 4 + 1 = 13
pub const NUM_ORDERS: usize = (MAX_ORDER - MIN_ORDER + 1) as usize;

/// 空闲标志：BuddyBlock.flags 的 bit 0
const BLOCK_FREE: u8 = 1 << 0;

/// Buddy 块头部 — 紧邻用户数据之前，8 字节。
///
/// 无论空闲还是已分配，每个 buddy 块都以 BuddyBlock 开头。
/// 用户数据区从 `block_ptr + 8` 开始。
///
/// ```text
/// ┌─────────────┐ ← block 地址（2^order 对齐）
/// │ order  (1B) │
/// │ flags  (1B) │  bit0: 1=空闲, 0=已分配
/// │ _resvd (6B) │
/// ├─────────────┤ ← user_ptr = block + 8（天然 8 字节对齐）
/// │  user data  │
/// └─────────────┘
/// ```
#[repr(C)]
pub struct BuddyBlock {
    /// 块阶数：4..=16
    pub order: u8,
    /// 标志位：bit0 = BLOCK_FREE
    pub flags: u8,
    _reserved: [u8; 6],
}

/// Buddy 分配器内部状态。
///
/// 由 `SpinLock<BuddyState>` 保护。核心操作（alloc_block、free、alloc_sized）
/// 为 [`BuddyState`] 的方法，调用者在持有锁后调用。
pub struct BuddyState {
    /// 空闲链表：free_lists[i] 指向 order = i + MIN_ORDER 的第一个空闲块。
    /// 空闲块间通过数据区前 8 字节的单向链表链接。
    pub free_lists: [*mut BuddyBlock; NUM_ORDERS],
    /// 堆基址 — buddy_of XOR 计算的基准
    pub heap_base: usize,
}

impl BuddyBlock {
    /// 读取空闲块链表中的下一个块指针。
    #[inline]
    pub unsafe fn next(block: *mut BuddyBlock) -> *mut BuddyBlock {
        let data = (block as *mut u8).add(BUDDY_HEADER_SIZE);
        ptr::read_unaligned(data as *const *mut BuddyBlock)
    }

    /// 设置空闲块链表的下一个块指针。
    #[inline]
    pub unsafe fn set_next(block: *mut BuddyBlock, next: *mut BuddyBlock) {
        let data = (block as *mut u8).add(BUDDY_HEADER_SIZE);
        ptr::write_unaligned(data as *mut *mut BuddyBlock, next);
    }
}

impl BuddyState {
    pub const fn new() -> Self {
        Self {
            free_lists: [ptr::null_mut(); NUM_ORDERS],
            heap_base: 0,
        }
    }

    /// 初始化 buddy 状态：将整个堆区域作为单个 order-16 空闲块。
    ///
    /// # Safety
    ///
    /// 必须在堆分配器初始化早期调用一次。`heap_start` 必须指向
    /// 4 KiB 对齐的、大小不小于 `1 << MAX_ORDER` 的内存区域。
    pub unsafe fn init(&mut self, heap_start: *mut u8) {
        self.heap_base = heap_start as usize;

        let block = heap_start as *mut BuddyBlock;
        (*block).order = MAX_ORDER;
        (*block).flags = 1; // BLOCK_FREE
        BuddyBlock::set_next(block, ptr::null_mut());

        self.free_lists[(MAX_ORDER - MIN_ORDER) as usize] = block;
    }

    /// 计算给定块的伙伴地址。
    #[inline]
    fn buddy_of(&self, block: *mut u8, order: u8) -> *mut u8 {
        let offset = (block as usize).wrapping_sub(self.heap_base);
        let buddy_offset = offset ^ (1usize << order);
        (self.heap_base + buddy_offset) as *mut u8
    }

    /// 从指定阶数的空闲链表中移除目标块。
    unsafe fn remove_free_block(&mut self, target: *mut BuddyBlock, order: u8) {
        let idx = (order - MIN_ORDER) as usize;
        let mut prev: *mut BuddyBlock = ptr::null_mut();
        let mut curr = self.free_lists[idx];

        while !curr.is_null() {
            if curr == target {
                let n = BuddyBlock::next(curr);
                if prev.is_null() {
                    self.free_lists[idx] = n;
                } else {
                    BuddyBlock::set_next(prev, n);
                }
                return;
            }
            prev = curr;
            curr = BuddyBlock::next(curr);
        }
    }

    /// 分配一个 2^order 字节的块。
    ///
    /// 返回指向 BuddyBlock 头部的指针（调用者负责计算用户数据偏移）。
    /// 若内存不足返回 null。
    ///
    /// # Safety
    ///
    /// 必须在持有 SpinLock 时调用。
    pub unsafe fn alloc_block(&mut self, order: u8) -> *mut u8 {
        if !(MIN_ORDER..=MAX_ORDER).contains(&order) {
            return ptr::null_mut();
        }

        let mut found_order = order;
        let mut block: *mut BuddyBlock = ptr::null_mut();

        while found_order <= MAX_ORDER {
            let idx = (found_order - MIN_ORDER) as usize;
            if !self.free_lists[idx].is_null() {
                block = self.free_lists[idx];
                self.free_lists[idx] = BuddyBlock::next(block);
                break;
            }
            found_order += 1;
        }

        if block.is_null() {
            return ptr::null_mut::<u8>();
        }

        // 逐级拆分
        while found_order > order {
            found_order -= 1;
            let buddy_ptr = ((block as usize) + (1usize << found_order)) as *mut BuddyBlock;

            (*buddy_ptr).order = found_order;
            (*buddy_ptr).flags = BLOCK_FREE;

            let idx = (found_order - MIN_ORDER) as usize;
            BuddyBlock::set_next(buddy_ptr, self.free_lists[idx]);
            self.free_lists[idx] = buddy_ptr;
        }

        (*block).order = order;
        (*block).flags = 0;

        block as *mut u8
    }

    /// 释放块并向上合并。
    ///
    /// # Safety
    ///
    /// `user_ptr` 必须是由 [`alloc_sized`](Self::alloc_sized) 返回的有效指针。
    /// 必须在持有 SpinLock 时调用。
    pub unsafe fn free(&mut self, user_ptr: *mut u8) {
        if user_ptr.is_null() {
            return;
        }

        let mut block = user_ptr.sub(BUDDY_HEADER_SIZE) as *mut BuddyBlock;
        let mut order = (*block).order;

        (*block).flags = BLOCK_FREE;

        // 向上合并
        while order < MAX_ORDER {
            let buddy_ptr = self.buddy_of(block as *mut u8, order) as *mut BuddyBlock;

            if (*buddy_ptr).flags & BLOCK_FREE == 0 {
                break;
            }
            if (*buddy_ptr).order != order {
                break;
            }

            self.remove_free_block(buddy_ptr, order);

            // 合并：低地址块存活
            if (buddy_ptr as usize) < (block as usize) {
                block = buddy_ptr;
            }
            order += 1;
            (*block).order = order;
        }

        let idx = (order - MIN_ORDER) as usize;
        BuddyBlock::set_next(block, self.free_lists[idx]);
        self.free_lists[idx] = block;
    }

    /// 按大小和对齐要求分配内存（面向 GlobalAlloc 的接口）。
    ///
    /// 返回用户数据区指针（已偏移 BUDDY_HEADER_SIZE 并对齐调整）。
    ///
    /// # Safety
    ///
    /// 必须在持有 SpinLock 时调用。
    pub unsafe fn alloc_sized(&mut self, size: usize, align: usize) -> *mut u8 {
        let total = BUDDY_HEADER_SIZE
            .saturating_add(align.saturating_sub(BUDDY_HEADER_SIZE))
            .saturating_add(size);

        let order = next_order(total);
        let order = order.max(next_order(align)).min(MAX_ORDER);

        let block_ptr = self.alloc_block(order);
        if block_ptr.is_null() {
            return ptr::null_mut();
        }

        let user = block_ptr.add(BUDDY_HEADER_SIZE);
        user.add(user.align_offset(align))
    }
}

/// 计算大于等于 `x` 的最小 2 的幂的阶数。
///
/// 例如：next_order(16) = 4, next_order(17) = 5。
pub fn next_order(x: usize) -> u8 {
    if x <= (1 << MIN_ORDER) {
        return MIN_ORDER;
    }
    x.next_power_of_two().trailing_zeros() as u8
}

/// Buddy 全局状态（自旋锁保护）
pub static BUDDY: SpinLock<BuddyState> = SpinLock::new(BuddyState::new());

/// 初始化 buddy 堆分配器：将整个 64 KiB 堆作为单个 order-16 空闲块。
///
/// # Safety
///
/// 必须在内核启动早期、单 hart 下调用一次。在任何堆分配之前调用。
pub unsafe fn init() {
    let heap_start = ptr::addr_of_mut!(HEAP.0) as *mut u8;
    BUDDY.lock(|state| state.init(heap_start));
}
