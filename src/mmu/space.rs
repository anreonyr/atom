// 地址空间 — MMU 子系统的核心抽象
//
// AddressSpace 拥有一个 Sv39 根页表，提供虚拟→物理映射、权限管理、
// 地址翻译等高层操作。内核半区（条目 256–511）可在地址空间之间共享。

use crate::mmu::addr::{PhysAddr, VirtAddr};
use crate::mmu::alloc::PageFrameAllocator;
use crate::mmu::entry::PteFlags;
use crate::mmu::table::{MapError, PageTable};
use crate::mmu::PAGE_SIZE;

/// Sv39 虚拟地址空间。
///
/// 拥有根页表（L2）的物理地址。内核半区（VPN[2] >= 256）可跨地址空间
/// 共享 —— 创建用户地址空间时通过 [`share_kernel_half`] 从内核空间复制。
///
/// # 并发
///
/// 单 hart 下，`map`/`unmap`/`protect` 通过 `&self` + 内部 raw pointer 操作：
/// 初始化期间无并发；运行时中断禁用，trap handler 内的 `translate` 与
/// 内核路径的 `map` 操作不会交错。
pub struct AddressSpace {
    root: PhysAddr,
}

impl AddressSpace {
    /// 创建一个全新的空地址空间（所有 PTE 无效）。
    ///
    /// 用完后必须调用 [`destroy`](Self::destroy) 释放页表内存。
    pub fn new(alloc: &dyn PageFrameAllocator) -> Result<Self, MapError> {
        let root_phys = alloc.alloc_frame().ok_or(MapError::OutOfMemory)?;
        // alloc_frame 已经零初始化，无需重复清零
        Ok(Self { root: root_phys })
    }

    /// 从另一个地址空间复制内核半区（L2 条目 256–511）。
    ///
    /// 用于创建用户地址空间时共享内核映射。
    pub fn share_kernel_half(&mut self, kernel: &AddressSpace) {
        let src = unsafe { &*(kernel.root.as_usize() as *const PageTable) };
        let dst = unsafe { &mut *(self.root.as_usize() as *mut PageTable) };
        dst[256..512].copy_from_slice(&src[256..512]);
    }

    // ── 映射操作 ──────────────────────────────────────────────

    /// 映射一段虚拟地址到物理地址。
    ///
    /// `vaddr` 和 `paddr` 必须页对齐，`size` 必须是 `PAGE_SIZE` 的整数倍。
    pub fn map(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn PageFrameAllocator,
    ) -> Result<(), MapError> {
        let root = unsafe { &mut *(self.root.as_usize() as *mut PageTable) };
        root.map_region(vaddr, paddr, size, flags, alloc)
    }

    /// 取消映射一段虚拟地址。不释放中间页表（惰性策略）。
    pub fn unmap(&self, vaddr: VirtAddr, size: usize) {
        let root = unsafe { &mut *(self.root.as_usize() as *mut PageTable) };
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            root.unmap(vaddr + i * PAGE_SIZE);
        }
    }

    /// 修改已映射区域的保护标志。
    ///
    /// 遍历区域内每一页，若叶子 PTE 存在则替换标志位。
    pub fn protect(
        &self,
        vaddr: VirtAddr,
        size: usize,
        flags: PteFlags,
    ) -> Result<(), MapError> {
        let root = unsafe { &mut *(self.root.as_usize() as *mut PageTable) };
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            if let Some(entry) = root.get_entry(va) {
                if entry.is_valid() && entry.is_leaf() {
                    // 通过 walk_mut 获取可变引用以修改标志位
                    let leaf = root.walk_mut(va, &crate::mmu::alloc::BootstrapPageAllocator)?;
                    leaf.set_flags(flags | PteFlags::V);
                }
            }
        }
        Ok(())
    }

    // ── 查询 ──────────────────────────────────────────────────

    /// 将虚拟地址翻译为物理地址和标志位。
    pub fn translate(&self, vaddr: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
        let root = unsafe { &*(self.root.as_usize() as *const PageTable) };
        root.walk(vaddr)
    }

    /// 获取根页表 PPN（写入 satp 用）。
    pub fn root_ppn(&self) -> u64 {
        (self.root.as_usize() >> crate::mmu::PAGE_SHIFT) as u64
    }

    // ── 生命周期 ──────────────────────────────────────────────

    /// 递归释放所有子页表（不包括根页表本身）。
    ///
    /// # Safety
    ///
    /// 调用后该地址空间无效，不可再使用。
    pub unsafe fn destroy(&mut self, alloc: &dyn PageFrameAllocator) {
        let root = &mut *(self.root.as_usize() as *mut PageTable);
        root.destroy_children(2, alloc);
        alloc.free_frame(self.root);
    }
}
