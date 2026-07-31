// Sv39 三级页表结构 — 页表遍历、映射、取消映射、帧分配/释放
//
// Sv39 地址分解：
//   VA[38:30] → VPN[2] — 根页表 (Level 2, L2) 索引
//   VA[29:21] → VPN[1] — 中间页表 (Level 1, L1) 索引
//   VA[20:12] → VPN[0] — 叶子页表 (Level 0, L0) 索引
//   VA[11:0]            — 页内偏移

use core::{
    alloc::{Allocator, Layout},
    ptr::NonNull,
};

use crate::memory::{
    addr::{PhysAddr, VirtAddr},
    entry::{PageTableEntry, PteFlags},
    PAGE_SHIFT, PAGE_SIZE,
};

/// 页表操作错误。
#[derive(Debug)]
pub(crate) enum MapError {
    /// 物理页帧分配器耗尽。
    OutOfMemory,
    /// 该虚拟地址已被映射。
    AlreadyMapped,
    /// 地址未按页对齐。
    NotAligned,
    /// 页表项/中间表不存在。
    NotMapped,
    /// 虚拟地址不在任何已注册的 Region 内。
    NoRegion,
}

/// Sv39 页表 — 512 条目 × 8 字节 = 4 KiB，对齐到页边界。
///
/// 不实现 `Clone` / `Copy`：4 KiB 的隐式复制是错误源。
/// `entries` 字段公开（`pub(crate)`），数组自带 `Index`/`IndexMut`/slice 操作——
/// 无需额外 trait impl。
#[repr(C, align(4096))]
pub(crate) struct PageTable {
    pub(crate) entries: [PageTableEntry; 512],
}

impl PageTable {
    /// 从分配器分配一个零页表。
    ///
    /// # Errors
    ///
    /// 物理帧耗尽时返回 [`MapError::OutOfMemory`]。
    pub(crate) fn allocate(alloc: &dyn Allocator) -> Result<NonNull<Self>, MapError> {
        alloc
            .allocate(Layout::new::<PageTable>())
            .map(|mem| {
                let ptr: NonNull<Self> = mem.cast();
                // SAFETY: 分配器刚给的独占页，清零以保证所有 PTE 初始无效
                unsafe {
                    core::ptr::write_bytes(ptr.as_ptr() as *mut u8, 0, PAGE_SIZE);
                }
                ptr
            })
            .map_err(|_| MapError::OutOfMemory)
    }

    /// 释放由 [`allocate`](Self::allocate) 分配的页表。
    ///
    /// # Safety
    ///
    /// `ptr` 必须由 `allocate` 分配且尚未释放。
    pub(crate) unsafe fn deallocate(ptr: NonNull<Self>, alloc: &dyn Allocator) {
        alloc.deallocate(ptr.cast::<u8>(), Layout::new::<PageTable>());
    }

    /// 只读遍历到叶子 PTE，返回物理地址和标志位。
    ///
    /// 中间表无效、遇到 superpage、或叶子无效时返回 `None`。
    pub(crate) fn walk_ref(&self, vaddr: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
        let l2 = &self.entries[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return None;
        }
        let p1 = unsafe { &*(l2.paddr() as *const PageTable) };

        let l1 = &p1.entries[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return None;
        }
        let p0 = unsafe { &*(l1.paddr() as *const PageTable) };

        let leaf = &p0.entries[vaddr.vpn(0)];
        if leaf.is_valid() && leaf.is_leaf() {
            Some((PhysAddr::from_raw(leaf.paddr() as usize), leaf.flags()))
        } else {
            None
        }
    }

    /// 遍历到叶子 PTE 并返回可变引用。
    ///
    /// `alloc` 为 `Some` 时按需分配中间页表；为 `None` 时遇到无效中间表
    /// 则返回 [`MapError::NotMapped`]。
    ///
    /// # Errors
    ///
    /// - `alloc` 为 `Some` 且物理帧耗尽 → [`MapError::OutOfMemory`]
    /// - `alloc` 为 `None` 且中间表无效 → [`MapError::NotMapped`]
    pub(crate) fn walk_mut(
        &mut self,
        vaddr: VirtAddr,
        alloc: Option<&dyn Allocator>,
    ) -> Result<&mut PageTableEntry, MapError> {
        // Level 2 → Level 1
        let l2 = &mut self.entries[vaddr.vpn(2)];
        if !l2.is_valid() {
            match alloc {
                Some(a) => {
                    let child = Self::allocate(a)?;
                    let child_pa = child.as_ptr() as usize;
                    l2.set((child_pa >> PAGE_SHIFT) as u64, PteFlags::V);
                }
                None => return Err(MapError::NotMapped),
            }
        }
        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };

        // Level 1 → Level 0
        let l1 = &mut p1.entries[vaddr.vpn(1)];
        if !l1.is_valid() {
            match alloc {
                Some(a) => {
                    let child = Self::allocate(a)?;
                    let child_pa = child.as_ptr() as usize;
                    l1.set((child_pa >> PAGE_SHIFT) as u64, PteFlags::V);
                }
                None => return Err(MapError::NotMapped),
            }
        }
        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };

        Ok(&mut p0.entries[vaddr.vpn(0)])
    }

    /// 映射 `size` 字节（n 页）从 `vaddr` 到 `paddr` 的连续区域。
    ///
    /// 按需分配中间页表节点。
    ///
    /// # Errors
    ///
    /// - [`MapError::NotAligned`] — 地址或大小未按 4 KiB 对齐。
    /// - [`MapError::AlreadyMapped`] — 任一虚拟地址已被映射。
    /// - [`MapError::OutOfMemory`] — 物理帧耗尽。
    pub(crate) fn map(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        if vaddr.offset() != 0 || !paddr.is_aligned() || size & (PAGE_SIZE - 1) != 0 {
            return Err(MapError::NotAligned);
        }

        let pages = size / PAGE_SIZE;
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            let pa = paddr + i * PAGE_SIZE;
            let leaf = self.walk_mut(va, Some(alloc))?;
            if leaf.is_valid() {
                return Err(MapError::AlreadyMapped);
            }
            let ppn = (pa.as_usize() >> PAGE_SHIFT) as u64;
            leaf.set(ppn, flags | PteFlags::V);
        }
        Ok(())
    }

    /// 取消映射一个虚拟地址。
    ///
    /// 将叶子 PTE 清零，不释放中间页表节点（惰性策略）。
    pub(crate) fn unmap(&mut self, vaddr: VirtAddr) {
        let l2 = &self.entries[vaddr.vpn(2)];
        if !l2.is_valid() || l2.is_leaf() {
            return;
        }

        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };
        let l1 = &p1.entries[vaddr.vpn(1)];
        if !l1.is_valid() || l1.is_leaf() {
            return;
        }

        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };
        p0.entries[vaddr.vpn(0)].clear();
    }

    /// 递归释放子树（不含自身）。
    ///
    /// `level` 为当前页表层级（2 = L2, 1 = L1, 0 = 叶子，不释放）。
    ///
    /// # Safety
    ///
    /// 调用后子树不再有效，不可再被访问。
    pub(crate) unsafe fn clean(&mut self, level: u8, alloc: &dyn Allocator) {
        if level == 0 {
            return;
        }
        for i in 0..512 {
            let entry = &mut self.entries[i];
            if entry.is_valid() && !entry.is_leaf() {
                let child_ptr = NonNull::new(entry.paddr() as *mut PageTable).unwrap();
                (*child_ptr.as_ptr()).clean(level - 1, alloc);
                Self::deallocate(child_ptr, alloc);
                entry.clear();
            }
        }
    }
}
