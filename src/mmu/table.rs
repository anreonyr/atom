// Sv39 三级页表结构 — 页表遍历、映射、取消映射、帧分配/释放
//
// Sv39 地址分解：
//   VA[38:30] → VPN[2] — 根页表 (Level 2, L2) 索引
//   VA[29:21] → VPN[1] — 中间页表 (Level 1, L1) 索引
//   VA[20:12] → VPN[0] — 叶子页表 (Level 0, L0) 索引
//   VA[11:0]            — 页内偏移

use core::alloc::{Allocator, Layout};
use core::ops::{Index, IndexMut, Range};
use core::ptr::NonNull;

use crate::mmu::addr::{PhysAddr, VirtAddr};
use crate::mmu::entry::PageTableEntry;
use crate::mmu::entry::PteFlags;
use crate::mmu::{PAGE_SHIFT, PAGE_SIZE};

/// 页表映射操作错误。
#[derive(Debug)]
pub enum MapError {
    /// 物理页帧分配器耗尽。
    OutOfMemory,
    /// 该虚拟地址已被映射。
    AlreadyMapped(VirtAddr),
    /// 地址未按页对齐。
    NotAligned,
}

/// Sv39 页表 — 512 条目 × 8 字节 = 4 KiB，对齐到页边界。
///
/// 不实现 `Clone` / `Copy`：4 KiB 的隐式复制是错误源。
#[repr(C, align(4096))]
pub struct PageTable(pub [PageTableEntry; 512]);

impl PageTable {
    /// 创建一个全零的新页表（所有条目均无效）。
    pub const fn new() -> Self {
        PageTable([PageTableEntry::empty(); 512])
    }
}

impl Index<usize> for PageTable {
    type Output = PageTableEntry;

    #[inline(always)]
    fn index(&self, idx: usize) -> &PageTableEntry {
        &self.0[idx]
    }
}

impl IndexMut<usize> for PageTable {
    #[inline(always)]
    fn index_mut(&mut self, idx: usize) -> &mut PageTableEntry {
        &mut self.0[idx]
    }
}

impl Index<Range<usize>> for PageTable {
    type Output = [PageTableEntry];

    #[inline(always)]
    fn index(&self, range: Range<usize>) -> &[PageTableEntry] {
        &self.0[range]
    }
}

impl IndexMut<Range<usize>> for PageTable {
    #[inline(always)]
    fn index_mut(&mut self, range: Range<usize>) -> &mut [PageTableEntry] {
        &mut self.0[range]
    }
}

impl PageTable {
    /// 分配一个 4 KiB 页表帧，返回物理地址。
    ///
    /// # Errors
    ///
    /// 物理帧耗尽时返回 [`MapError::OutOfMemory`]。
    pub(crate) fn alloc_page(alloc: &dyn Allocator) -> Result<usize, MapError> {
        alloc
            .allocate(Layout::new::<PageTable>())
            .map(|f| f.as_ptr() as *const u8 as usize)
            .map_err(|_| MapError::OutOfMemory)
    }

    /// 释放一个 4 KiB 页表帧。
    ///
    /// # Safety
    ///
    /// `pa` 必须是由 [`alloc_page`](Self::alloc_page) 分配的地址。
    pub(crate) fn dealloc_page(alloc: &dyn Allocator, pa: usize) {
        unsafe {
            let ptr = NonNull::new(pa as *mut u8).unwrap();
            alloc.deallocate(ptr, Layout::new::<PageTable>());
        }
    }

    /// 只读遍历到叶子 PTE 并返回引用。
    ///
    /// 中间表或叶子无效时返回 `None`；遇到 superpage 同样返回 `None`。
    pub fn get_entry(&self, vaddr: VirtAddr) -> Option<&PageTableEntry> {
        let l2 = &self[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return None;
        }
        let p1 = unsafe { &*(l2.paddr() as *const PageTable) };

        let l1 = &p1[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return None;
        }
        let p0 = unsafe { &*(l1.paddr() as *const PageTable) };

        Some(&p0[vaddr.vpn(0)])
    }

    /// 只读遍历到叶子 PTE，返回物理地址和标志位。
    ///
    /// 中间表无效、遇到 superpage、或叶子无效时返回 `None`。
    pub fn walk(&self, vaddr: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
        let l2 = &self[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return None;
        }
        let p1 = unsafe { &*(l2.paddr() as *const PageTable) };

        let l1 = &p1[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return None;
        }
        let p0 = unsafe { &*(l1.paddr() as *const PageTable) };

        let leaf = &p0[vaddr.vpn(0)];
        if leaf.is_valid() && leaf.is_leaf() {
            Some((PhysAddr::from_raw(leaf.paddr() as usize), leaf.flags()))
        } else {
            None
        }
    }

    /// 遍历到叶子 PTE，按需分配中间页表。
    ///
    /// 返回叶子 PTE 的可变引用。
    ///
    /// # Errors
    ///
    /// 物理帧耗尽时返回 [`MapError::OutOfMemory`]。
    pub fn walk_mut(
        &mut self,
        vaddr: VirtAddr,
        alloc: &dyn Allocator,
    ) -> Result<&mut PageTableEntry, MapError> {
        // Level 2 → Level 1
        let l2 = &mut self[vaddr.vpn(2)];
        if !l2.is_valid() {
            let pa = Self::alloc_page(alloc)?;
            l2.set((pa >> PAGE_SHIFT) as u64, PteFlags::V);
        }
        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };

        // Level 1 → Level 0
        let l1 = &mut p1[vaddr.vpn(1)];
        if !l1.is_valid() {
            let pa = Self::alloc_page(alloc)?;
            l1.set((pa >> PAGE_SHIFT) as u64, PteFlags::V);
        }
        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };

        Ok(&mut p0[vaddr.vpn(0)])
    }

    /// 遍历到已存在的叶子 PTE，不分配中间页表。
    ///
    /// 中间表或叶子无效时返回 `None`。
    pub fn walk_mut_existing(&mut self, vaddr: VirtAddr) -> Option<&mut PageTableEntry> {
        let l2 = &mut self[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return None;
        }
        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };

        let l1 = &mut p1[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return None;
        }
        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };

        let leaf = &mut p0[vaddr.vpn(0)];
        if leaf.is_valid() {
            Some(leaf)
        } else {
            None
        }
    }

    /// 映射一个 4 KiB 页面到页表。
    ///
    /// 按需分配中间页表节点。
    ///
    /// # Errors
    ///
    /// - [`MapError::NotAligned`] — `vaddr` 或 `paddr` 未按 4 KiB 对齐。
    /// - [`MapError::AlreadyMapped`] — 虚拟地址已被映射。
    /// - [`MapError::OutOfMemory`] — 物理帧耗尽。
    pub fn map_page(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        if vaddr.offset() != 0 || !paddr.is_aligned() {
            return Err(MapError::NotAligned);
        }

        let leaf = self.walk_mut(vaddr, alloc)?;
        if leaf.is_valid() {
            return Err(MapError::AlreadyMapped(vaddr));
        }

        let ppn = (paddr.as_usize() >> PAGE_SHIFT) as u64;
        leaf.set(ppn, flags | PteFlags::V);
        Ok(())
    }

    /// 映射一段连续内存区域。
    ///
    /// 以 4 KiB 页为粒度逐页映射。
    ///
    /// # Errors
    ///
    /// 任一页映射失败即返回错误。
    pub fn map_region(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            self.map_page(vaddr + i * PAGE_SIZE, paddr + i * PAGE_SIZE, flags, alloc)?;
        }
        Ok(())
    }

    /// 取消映射一个虚拟地址。
    ///
    /// 将叶子 PTE 清零，不释放中间页表节点（惰性策略）。
    pub fn unmap(&mut self, vaddr: VirtAddr) {
        let l2 = &self[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return;
        }

        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };
        let l1 = &p1[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return;
        }

        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };
        p0[vaddr.vpn(0)].clear();
    }

    /// 递归释放所有子页表。
    ///
    /// `level` 为当前页表层级（2 = L2, 1 = L1, 0 = 叶子，不释放）。
    /// 不包括根页表本身。
    ///
    /// # Safety
    ///
    /// 调用后该页表子树不再有效，不可再被访问。
    pub unsafe fn destroy_children(&mut self, level: u8, alloc: &dyn Allocator) {
        if level == 0 {
            return;
        }
        for i in 0..512 {
            let entry = &mut self[i];
            if entry.is_valid() && !entry.is_leaf() {
                let child = &mut *(entry.paddr() as *mut PageTable);
                child.destroy_children(level - 1, alloc);
                Self::dealloc_page(alloc, entry.paddr() as usize);
                entry.clear();
            }
        }
    }
}
