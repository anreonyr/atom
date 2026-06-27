// Slab 对象缓存分配器 — 为常见小对象提供 O(1) 分配/释放
//
// 维护 5 个固定大小缓存（32/64/128/256/512 字节），每个缓存由若干 4 KiB
// "slab 页" 组成。每页首部存放 SlabPage 元数据，剩余空间均分为对象槽位。
// 空闲对象通过单向链表链接（指针存于对象本身的前 8 字节）。
//
// # 独立性
//
// 本模块完全独立于 buddy 分配器，使用自己的静态页池（.bss 节）。
// 当页池耗尽时 slab_alloc 返回 null，由上层 GlobalAlloc 回退到 buddy。
//
// 并发安全：所有操作通过 SpinLock<SlabState> 保护。

use core::ptr;

use crate::lock::SpinLock;


/// Slab 页大小阶数：2^12 = 4 KiB
const SLAB_PAGE_ORDER: u8 = 12;
const SLAB_PAGE_SIZE: usize = 1 << SLAB_PAGE_ORDER;

/// Slab 页头部大小（含对齐填充）
pub const SLAB_HEADER_SIZE: usize = 24;

/// 缓存数量
pub const NUM_CACHES: usize = 5;

/// 缓存对象大小表（升序）
pub const CACHE_SIZES: [u16; NUM_CACHES] = [32, 64, 128, 256, 512];

/// 页池容量：6 页 = 24 KiB（每个缓存至少 1 页）
const POOL_PAGES: usize = 6;

/// Slab 页对象区起始偏移：页起始 + 24 字节
const OBJ_OFFSET: usize = SLAB_HEADER_SIZE;

/// 每页可用对象区大小
const OBJ_AREA: usize = SLAB_PAGE_SIZE - SLAB_HEADER_SIZE;


/// 静态页池 — 放在 .bss，4 KiB 对齐
#[repr(align(4096))]
struct SlabPool([u8; POOL_PAGES * SLAB_PAGE_SIZE]);

static mut POOL: SlabPool = SlabPool([0; POOL_PAGES * SLAB_PAGE_SIZE]);


/// Slab 页头部 — 位于每个 4 KiB 页的前 24 字节。
///
/// ```text
/// ┌──────────────┐ ← page 地址（4 KiB 对齐）
/// │ free_list    │  8B — 本页空闲对象链表头
/// │ next         │  8B — 同缓存下一个部分满页
/// │ free_count   │  2B — 本页剩余空闲对象数
/// │ obj_size     │  2B — 本页对象大小
/// │ [pad]        │  4B — 对齐到 24B
/// ├──────────────┤ ← 对象区起点（24 ≡ 0 mod 8）
/// │   object 0   │
/// │   object 1   │
/// │     ...      │
/// └──────────────┘
/// ```
#[repr(C)]
pub struct SlabPage {
    /// 本页空闲对象链表头
    pub free_list: *mut u8,
    /// 同缓存下一个部分满页（至少有一个空闲对象）
    pub next: *mut SlabPage,
    /// 本页剩余空闲对象数
    pub free_count: u16,
    /// 本页对象大小
    pub obj_size: u16,
}


/// 单个对象大小缓存
pub struct SlabCache {
    /// 此缓存的对象大小
    pub obj_size: u16,
    /// 部分满页链表头
    pub partial: *mut SlabPage,
}

/// Slab 分配器内部状态。
pub struct SlabState {
    pub caches: [SlabCache; NUM_CACHES],
    /// 页池游标 — 下一个可用页的索引
    pool_cursor: usize,
}

impl SlabState {
    pub const fn new() -> Self {
        Self {
            caches: [
                SlabCache {
                    obj_size: 32,
                    partial: ptr::null_mut(),
                },
                SlabCache {
                    obj_size: 64,
                    partial: ptr::null_mut(),
                },
                SlabCache {
                    obj_size: 128,
                    partial: ptr::null_mut(),
                },
                SlabCache {
                    obj_size: 256,
                    partial: ptr::null_mut(),
                },
                SlabCache {
                    obj_size: 512,
                    partial: ptr::null_mut(),
                },
            ],
            pool_cursor: 0,
        }
    }

    /// 从页池分配一个新页。池耗尽时返回 null。
    fn alloc_page(&mut self) -> *mut u8 {
        if self.pool_cursor >= POOL_PAGES {
            return ptr::null_mut();
        }
        // SAFETY: pool_cursor 严格递增，每个索引仅分配一次，无并发
        let page = unsafe {
            (ptr::addr_of_mut!(POOL.0) as *mut u8).add(self.pool_cursor * SLAB_PAGE_SIZE)
        };
        self.pool_cursor += 1;
        page
    }

    /// 从 Slab 缓存分配一个对象。
    ///
    /// 若当前缓存无可用页且页池已耗尽，返回 null。
    /// 调用者（GlobalAlloc）应回退到 buddy 分配器。
    ///
    /// # Safety
    ///
    /// 必须在持有 `SlabState` 的 SpinLock 时调用。
    pub unsafe fn alloc(&mut self, ci: usize) -> *mut u8 {
        // 1. 尝试从部分满页分配
        let partial = self.caches[ci].partial;
        if !partial.is_null() {
            let page = partial;
            let obj = (*page).free_list;

            // 弹出：free_list = obj.next
            (*page).free_list = ptr::read_unaligned(obj as *const *mut u8);
            (*page).free_count -= 1;

            // 若页变满，从 partial 链表移除
            if (*page).free_count == 0 {
                self.caches[ci].partial = (*page).next;
            }

            return obj;
        }

        // 2. 无可用页：从页池申请新页
        let page_ptr = self.alloc_page();
        if page_ptr.is_null() {
            return ptr::null_mut::<u8>();
        }

        let obj_size = self.caches[ci].obj_size;
        let page = SlabPage::init(page_ptr, obj_size);
        let obj = (*page).free_list;

        // 弹出第一个对象
        (*page).free_list = ptr::read_unaligned(obj as *const *mut u8);
        (*page).free_count -= 1;

        // 链入 partial 链表
        (*page).next = self.caches[ci].partial;
        self.caches[ci].partial = page;

        obj
    }

    /// 将对象归还给 Slab 缓存。
    ///
    /// # Safety
    ///
    /// `ptr` 必须是由同大小缓存的 [`alloc`](Self::alloc) 返回的有效指针。
    /// 必须在持有 `SlabState` 的 SpinLock 时调用。
    pub unsafe fn free(&mut self, ptr: *mut u8, ci: usize) {
        if ptr.is_null() {
            return;
        }

        let cache = &mut self.caches[ci];

        // 通过掩码定位所属 SlabPage（页起始地址 = ptr & !(SLAB_PAGE_SIZE - 1)）
        let page = ((ptr as usize) & !(SLAB_PAGE_SIZE - 1)) as *mut SlabPage;
        let was_full = (*page).free_count == 0;

        // 将对象推回空闲链表
        ptr::write_unaligned(ptr as *mut *mut u8, (*page).free_list);
        (*page).free_list = ptr;
        (*page).free_count += 1;

        // 若之前满页，重新链入 partial 链表
        if was_full {
            (*page).next = cache.partial;
            cache.partial = page;
        }
    }
}

impl SlabPage {
    /// 初始化一个 slab 页：写入 SlabPage 头部，构建所有对象的空闲链表。
    ///
    /// # Safety
    ///
    /// `page_ptr` 必须指向 4 KiB 对齐、可写的内存。
    unsafe fn init(page_ptr: *mut u8, obj_size: u16) -> *mut SlabPage {
        let count = OBJ_AREA / obj_size as usize;
        let page = page_ptr as *mut SlabPage;

        // 从最后一个对象倒序构建单向链表（首次分配拿到第一个对象）
        let mut free_head: *mut u8 = ptr::null_mut();
        let obj_base = page_ptr.add(OBJ_OFFSET);

        for i in (0..count).rev() {
            let obj = obj_base.add(i * obj_size as usize);
            ptr::write_unaligned(obj as *mut *mut u8, free_head);
            free_head = obj;
        }

        (*page).free_list = free_head;
        (*page).next = ptr::null_mut();
        (*page).free_count = count as u16;
        (*page).obj_size = obj_size;

        page
    }
}


/// Slab 全局状态（自旋锁保护，const fn 初始化）
pub static SLAB: SpinLock<SlabState> = SpinLock::new(SlabState::new());


/// 查找能容纳 `size` 字节的最小缓存索引，若不存在返回 None。
///
/// `align` 必须 ≤ 8（slab 对象仅保证 8 字节对齐）。
pub fn cache_index(size: usize, align: usize) -> Option<usize> {
    if align > 8 {
        return None;
    }
    for (i, &obj_size) in CACHE_SIZES.iter().enumerate() {
        if size <= obj_size as usize {
            return Some(i);
        }
    }
    None
}
