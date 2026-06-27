// Sv39 三级页表结构 — 页表遍历、映射、取消映射
//
// Sv39 地址分解：
//   VA[38:30] → VPN[2] — 根页表 (Level 2, L2) 索引
//   VA[29:21] → VPN[1] — 中间页表 (Level 1, L1) 索引
//   VA[20:12] → VPN[0] — 叶子页表 (Level 0, L0) 索引
//   VA[11:0]            — 页内偏移

use crate::mmu::pte::PageTableEntry;

/// Sv39 页表 — 512 × 8 bytes = 4 KiB，对齐到页边界
#[repr(C, align(4096))]
#[derive(Clone, Copy)]
pub struct PageTable {
    pub entries: [PageTableEntry; 512],
}

/// 页大小 (4 KiB)
pub const PAGE_SIZE: usize = 4096;
/// 页偏移位数
pub const PAGE_SHIFT: usize = 12;

// ── VPN 提取 ────────────────────────────────────────────────

/// 从虚拟地址提取指定级别的 VPN（9 位索引）
///
/// level 2 → bits 38:30, level 1 → bits 29:21, level 0 → bits 20:12
#[inline(always)]
pub fn vpn(level: usize, vaddr: usize) -> usize {
    (vaddr >> (PAGE_SHIFT + level * 9)) & 0x1FF
}

impl PageTable {
    /// 创建一个全零的新页表（所有条目均无效）
    pub const fn new() -> Self {
        PageTable {
            entries: [PageTableEntry::empty(); 512],
        }
    }

    /// walk: 只读遍历到指定虚拟地址的叶子 PTE
    ///
    /// 如果中间表遇到无效项或叶子节点（superpage），返回 None。
    pub fn walk(&self, vaddr: usize) -> Option<&PageTableEntry> {
        let l2 = &self.entries[vpn(2, vaddr)];
        if !l2.is_valid() || l2.is_leaf() {
            return None;
        }
        let p1 = unsafe { &*(l2.paddr() as *const PageTable) };

        let l1 = &p1.entries[vpn(1, vaddr)];
        if !l1.is_valid() || l1.is_leaf() {
            return None;
        }
        let p0 = unsafe { &*(l1.paddr() as *const PageTable) };

        Some(&p0.entries[vpn(0, vaddr)])
    }

    /// 映射一个 4 KiB 页面到页表（按需分配中间节点）
    ///
    /// # Safety
    ///
    /// `paddr` 和 `vaddr` 必须 4 KiB 对齐。调用者需确保页表内存有效性。
    pub unsafe fn map_page(root: *mut PageTable, vaddr: usize, paddr: usize, flags: u64) {
        let ppn = (paddr >> PAGE_SHIFT) as u64;

        // Level 2 → Level 1 中间表
        let p2 = unsafe { &mut *root };
        let mut l2 = p2.entries[vpn(2, vaddr)];
        if !l2.is_valid() {
            let child = crate::mmu::alloc_table();
            let child_ppn = (child as usize >> PAGE_SHIFT) as u64;
            l2.set(child_ppn, PageTableEntry::V);
        }

        // Level 1 → Level 0 叶子表
        let p1 = unsafe { &mut *(l2.paddr() as *mut PageTable) };
        let mut l1 = p1.entries[vpn(1, vaddr)];
        if !l1.is_valid() {
            let child = crate::mmu::alloc_table();
            let child_ppn = (child as usize >> PAGE_SHIFT) as u64;
            l1.set(child_ppn, PageTableEntry::V);
        }

        // Level 0 — 写入叶子 PTE
        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };
        p0.entries[vpn(0, vaddr)].set(ppn, flags | PageTableEntry::V);
    }

    /// 映射一段连续内存区域（以 4 KiB 页为粒度）
    ///
    /// # Safety
    ///
    /// 调用者需确保 `vaddr`/`paddr` 页对齐，`size` > 0，
    /// 且映射的物理地址范围不与其他映射冲突。
    pub unsafe fn map_region(
        root: *mut PageTable,
        vaddr: usize,
        paddr: usize,
        size: usize,
        flags: u64,
    ) {
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            Self::map_page(root, vaddr + i * PAGE_SIZE, paddr + i * PAGE_SIZE, flags);
        }
    }

    /// 取消映射一个虚拟地址（将叶子 PTE 清零）
    ///
    /// 不释放中间页表节点。
    pub unsafe fn unmap(root: *mut PageTable, vaddr: usize) {
        let p2 = unsafe { &*root };
        let l2 = p2.entries[vpn(2, vaddr)];
        if !l2.is_valid() || l2.is_leaf() {
            return;
        }

        let p1 = unsafe { &*(l2.paddr() as *const PageTable) };
        let l1 = p1.entries[vpn(1, vaddr)];
        if !l1.is_valid() || l1.is_leaf() {
            return;
        }

        let p0 = unsafe { &mut *(l1.paddr() as *mut PageTable) };
        p0.entries[vpn(0, vaddr)].clear();
    }

    /// 刷新 TLB（sfence.vma）
    ///
    /// 映射/取消映射完成后必须调用一次。
    #[inline(always)]
    pub unsafe fn sfence() {
        core::arch::asm!("sfence.vma zero, zero");
    }
}
