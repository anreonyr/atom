// 地址空间 — MMU 子系统的核心抽象
//
// AddressSpace 拥有一个 Sv39 根页表，提供虚拟→物理映射、权限管理、
// 地址翻译等高层操作。内核半区（条目 256–511）可在地址空间之间共享。

use core::alloc::Allocator;
use core::ptr::NonNull;

use alloc::vec::Vec;

use crate::{
    lock::RelLock,
    memory::{
        addr::{PhysAddr, VirtAddr},
        entry::PteFlags,
        flush_tlb,
        table::{MapError, PageTable},
        PAGE_SIZE,
    },
    platform,
};

/// 虚拟内存区域（Region）类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// 匿名映射 — 缺页时分配零页
    Anonymous,
    /// 预留区域 — 不可访问，缺页时返回错误
    #[allow(dead_code)] // fault.rs 处理其缺页语义；当前无 Reserved 区域实例
    Reserved,
}

/// 虚拟内存区域 — 连续虚拟地址范围
#[derive(Debug, Clone)]
pub struct Region {
    pub start: usize,
    pub end: usize,
    pub flags: PteFlags,
    pub kind: RegionKind,
}

/// Sv39 虚拟地址空间。
///
/// 拥有根页表的物理内存。内核半区（VPN\[2\] >= 256）可跨地址空间
/// 共享——创建用户地址空间时通过 [`share_kernel`] 从内核空间复制。
///
/// # Concurrency
///
/// 单 hart 下，`map`/`protect` 通过 `&mut self`（COW 需更新 `shared_l2`，
/// 见 [`map`](Self::map)）；`unmap` 因 Region 表可变取 `&mut self`。
/// 初始化期间无并发；运行时中断禁用，trap handler 内的 `translate` 与
/// 内核路径的操作不会交错。
///
/// # Drop
///
/// 释放时递归回收所有页表帧（根表 + 中间表）到页分配器。
pub struct AddressSpace {
    root: NonNull<PageTable>,
    regions: Vec<Region>,
    /// 来自内核根表的共享 L2 索引（from_kernel 浅克隆，归 KERNEL_SPACE 所有）；
    /// drop 时 clean 跳过这些子树，避免释放共享页表。
    shared_l2: Vec<usize>,
}

// SAFETY: 单 hart 内核，地址空间由 RelLock 保护，不存在跨 hart 并发访问。
unsafe impl Send for AddressSpace {}
unsafe impl Sync for AddressSpace {}

impl AddressSpace {
    /// # Safety
    ///
    /// 地址空间必须已初始化（root 指向有效的 PageTable）。
    unsafe fn root_mut(&self) -> &'static mut PageTable {
        unsafe { &mut *self.root.as_ptr() }
    }

    /// # Safety
    ///
    /// 地址空间必须已初始化（root 指向有效的 PageTable）。
    unsafe fn root_ref(&self) -> &'static PageTable {
        unsafe { &*self.root.as_ptr() }
    }

    // ── 生命周期 ──────────────────────────────────────────────

    /// 创建一个全新的空地址空间，分配根页表帧。
    ///
    /// # Errors
    ///
    /// 物理帧耗尽时返回 [`MapError::OutOfMemory`]。
    pub fn new(alloc: &dyn Allocator) -> Result<Self, MapError> {
        let root = PageTable::allocate(alloc)?;
        Ok(Self {
            root,
            regions: Vec::new(),
            shared_l2: Vec::new(),
        })
    }

    /// 从内核地址空间创建新空间（继承全部页表条目）。
    ///
    /// 复制内核地址空间的完整根页表（用户半区 + 内核半区），
    /// 使新空间共享所有内核映射和 MMIO 恒等映射。
    ///
    /// # Errors
    ///
    /// 根页表分配失败时返回 [`MapError::OutOfMemory`]。
    pub fn from_kernel(alloc: &dyn Allocator) -> Result<Self, MapError> {
        let mut space = Self::new(alloc)?;
        let guard = KERNEL_SPACE.lock();
        if let Some(ref ks) = *guard {
            let src = unsafe { ks.root_ref() };
            let dst = unsafe { space.root_mut() };
            // 复制内核根表有效条目并记录共享索引：这些子树（DRAM identity / MMIO /
            // 高半区）归 KERNEL_SPACE 所有，本空间 drop 时 clean 必须跳过，
            // 否则浅克隆的共享页表会被误释放（use-after-free）。
            let mut shared = Vec::new();
            for i in 0..512 {
                if src.entries[i].is_valid() {
                    dst.entries[i] = src.entries[i];
                    shared.push(i);
                }
            }
            space.shared_l2 = shared;
        }
        Ok(space)
    }

    // ── 映射操作 ──────────────────────────────────────────────

    /// 映射 `size` 字节虚拟地址到物理地址（唯一公共映射入口）。
    ///
    /// 纯页表操作：仅安装 PTE，不注册 Region。按需分配中间页表。
    ///
    /// **vaddr、paddr、size 必须全部按 [`PAGE_SIZE`] 对齐**。
    /// 非对齐大小的调用方（如 MMIO 设备映射）须自行向上取整。
    ///
    /// # Copy-on-write（共享 L2 子树）
    ///
    /// `from_kernel` 浅克隆的地址空间共享 KERNEL_SPACE 的 L2 子树（`shared_l2`）。
    /// `PageTable::map` 会写叶子 PTE——直接写共享 L1/L0 表会污染 KERNEL_SPACE
    /// 及所有后续克隆空间（同一 boot 内第二个任务 map 同 VA 报 AlreadyMapped）。
    /// 故 map 前逐页检查目标 L2：命中共享先 [`cow_l2`] 整棵复制为私有再写。
    ///
    /// # Errors
    ///
    /// 参见 [`PageTable::map`]。
    pub fn map(
        &mut self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        // COW：涉及的每个 L2 子树若与内核共享，先复制为私有。
        // 同一 L2 多页重复命中：首次 cow 后 shared_l2 移除，后续 contains 为 false。
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let l2_idx = (vaddr + i * PAGE_SIZE).vpn(2);
            if self.shared_l2.contains(&l2_idx) {
                self.cow_l2(l2_idx, alloc)?;
            }
        }
        // SAFETY: 地址空间已初始化，map 只修改页表
        unsafe { self.root_mut().map(vaddr, paddr, size, flags, alloc)? };
        // Flush TLB so the newly installed mappings are visible immediately.
        // SAFETY: executed in S-mode; sfence.vma is always legal.
        unsafe {
            flush_tlb();
        }
        Ok(())
    }

    /// 复制共享 L2 子树为私有（COW），供后续 map 安全修改。
    ///
    /// `l2_idx` 必须位于 [`shared_l2`](Self::shared_l2)（from_kernel 浅克隆的
    /// 内核 L2 子树：MMIO / DRAM identity / 高半区）。复制 L1 表 + 其中所有
    /// 有效 L0 表，L2 条目指向私有副本，并从 shared_l2 移除该索引——
    /// clean()/Drop 随后按私有子树整棵释放（不误释放仍被 KERNEL_SPACE 引用的
    /// 共享 L0，也不泄漏）。
    ///
    /// 必须整棵复制而非仅路径：map 写入的叶子可深达 L0，且 Drop 的 skip 记录
    /// 是 L2 级——只复制 L1 会让共享 L0 既被本空间释放又被内核引用
    /// （use-after-free）。
    ///
    /// # Errors
    ///
    /// 中间表物理帧耗尽时返回 [`MapError::OutOfMemory`]；中途失败会留下部分
    /// 复制的私有表（后续 map 复用，无损坏）。
    fn cow_l2(&mut self, l2_idx: usize, alloc: &dyn Allocator) -> Result<(), MapError> {
        // 1. 复制 L1 表（源 = 共享 L2 条目指向的 L1，含所有 L0 指针）
        let new_l1 = PageTable::allocate(alloc)?;
        let new_l1_pa = new_l1.as_ptr() as usize;
        // SAFETY: root 有效；L2 条目有效且共享，paddr 指向合法 L1 表。
        let src_l1 = unsafe { self.root_ref() }.entries[l2_idx].paddr() as *const PageTable;
        // SAFETY: 源 L1 有效（L2 条目有效且共享）；目标页刚分配独占。
        unsafe {
            core::ptr::copy_nonoverlapping(src_l1, new_l1_pa as *mut PageTable, 1);
        }

        // 2. 复制 L1 中所有有效 L0 表（branch 条目指向的共享 L0）
        let l1 = unsafe { &mut *new_l1.as_ptr() };
        for i in 0..512 {
            if l1.entries[i].is_valid() && !l1.entries[i].is_leaf() {
                let new_l0 = PageTable::allocate(alloc)?;
                let new_l0_pa = new_l0.as_ptr() as usize;
                let src_l0 = l1.entries[i].paddr() as *const PageTable;
                // SAFETY: 源 L0 有效（L1 条目 branch）；目标页刚分配独占。
                unsafe {
                    core::ptr::copy_nonoverlapping(src_l0, new_l0_pa as *mut PageTable, 1);
                }
                l1.entries[i].set(
                    (new_l0_pa >> crate::memory::PAGE_SHIFT) as u64,
                    l1.entries[i].flags(),
                );
            }
        }

        // 3. L2 条目指向新 L1（保留 flags），记录为私有（clean/Drop 不再 skip）
        // SAFETY: root 有效；L2 条目有效，flags 读取安全。
        let flags = unsafe { self.root_ref() }.entries[l2_idx].flags();
        // SAFETY: root 有效。
        unsafe {
            self.root_mut().entries[l2_idx].set(
                (new_l1_pa >> crate::memory::PAGE_SHIFT) as u64,
                flags,
            );
        }
        self.shared_l2.retain(|&i| i != l2_idx);
        Ok(())
    }

    /// 取消映射一段虚拟地址并移除其 Region 记录（ecall munmap 后端）。
    ///
    /// 页表侧逐页清叶子 PTE（惰性策略，不释放中间页表）；Region 侧按重叠
    /// 删除与 `[start, start+size)` 相交的所有记录。`vaddr`/`size` 不要求
    /// 页对齐（向上取整语义与 POSIX munmap 一致）。
    #[allow(dead_code)] // ecall munmap 后端
    pub fn unmap(&mut self, vaddr: VirtAddr, size: usize) {
        let start = vaddr.as_usize();
        let end = start + size;

        // 页表侧：逐页清叶子 PTE
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            // SAFETY: 地址空间已初始化，unmap 只清零叶子 PTE
            unsafe { self.root_mut().unmap(vaddr + i * PAGE_SIZE) };
        }

        // Region 侧：删重叠记录
        self.regions.retain(|r| !(start < r.end && end > r.start));

        // SAFETY: executed in S-mode; sfence.vma is always legal.
        unsafe {
            flush_tlb();
        }
    }

    /// 修改已映射区域的保护标志。
    ///
    /// 单次遍历，不分配中间表——叶子 PTE 不存在则返回错误。
    ///
    /// # Errors
    ///
    /// 任一页的叶子 PTE 不存在时返回 [`MapError::NotMapped`]。
    #[allow(dead_code)] // ecall mprotect 后端预留
    pub fn protect(&self, vaddr: VirtAddr, size: usize, flags: PteFlags) -> Result<(), MapError> {
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            // SAFETY: 地址空间已初始化，protect 只修改已有叶子 PTE 标志位
            let leaf = unsafe { self.root_mut().walk_mut(va, None)? };
            leaf.set_flags(flags | PteFlags::V);
        }
        // SAFETY: executed in S-mode; sfence.vma is always legal.
        unsafe {
            flush_tlb();
        }
        Ok(())
    }

    // ── 缺页处理 ──────────────────────────────────────────────

    /// 缺页处理：查 Region → 分配零页 → 映射。
    ///
    /// 从 frame 分配器逐页取物理帧，清零后映射到 `vaddr` 起始的连续区间。
    /// 必须在目标地址已注册 Anonymous Region 时调用。
    ///
    /// # Errors
    ///
    /// - [`MapError::NoRegion`] — 地址不在任何 Region 内
    /// - [`MapError::OutOfMemory`] — 物理帧耗尽
    pub fn page_fault(
        &mut self,
        vaddr: VirtAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        // 前提：Region 已存在
        self.region_find(vaddr).ok_or(MapError::NoRegion)?;

        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            let layout = core::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
            let page = alloc.allocate(layout).map_err(|_| MapError::OutOfMemory)?;
            // SAFETY: 分配器刚给的独占页，清零以保证安全
            unsafe {
                core::ptr::write_bytes(page.as_ptr() as *mut u8, 0, PAGE_SIZE);
            }
            let pa = PhysAddr::from_raw(page.as_ptr() as *mut u8 as usize);
            self.map(va, pa, PAGE_SIZE, flags, alloc)?;
        }
        Ok(())
    }

    // ── 查询 ──────────────────────────────────────────────────

    /// 将虚拟地址翻译为物理地址和标志位。
    ///
    /// 未映射时返回 `None`。
    pub fn translate(&self, vaddr: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
        // SAFETY: address space is initialized; read-only traversal.
        unsafe { self.root_ref().walk_ref(vaddr).ok() }
    }

    /// 返回根页表页号（写入 `satp` 用）。
    pub fn root_page(&self) -> usize {
        self.root.as_ptr() as usize >> crate::memory::PAGE_SHIFT
    }

    // ── 地址空间共享 ──────────────────────────────────────────

    /// 从另一个地址空间复制内核半区（L2 条目 256–511）。
    ///
    /// 用于创建用户地址空间时共享内核映射。
    #[allow(dead_code)] // ecall fork 后端预留
    pub fn share_kernel(&mut self, kernel: &AddressSpace) {
        // SAFETY: 内核地址空间已初始化
        let src = unsafe { kernel.root_ref() };
        let dst = unsafe { self.root_mut() };
        dst.entries[256..512].copy_from_slice(&src.entries[256..512]);
        // 同步记录共享索引：这些高半区子树 drop 时不得释放。
        for i in 256..512 {
            if src.entries[i].is_valid() && !self.shared_l2.contains(&i) {
                self.shared_l2.push(i);
            }
        }
    }

    // ── Region 管理 ───────────────────────────────────────────

    /// 注册一段虚拟内存区域。
    ///
    /// `start` 和 `size` 必须 `PAGE_SIZE` 对齐，不得与已有 Region 重叠。
    pub fn region_add(
        &mut self,
        start: usize,
        size: usize,
        flags: PteFlags,
        kind: RegionKind,
    ) -> Result<(), MapError> {
        if !start.is_multiple_of(PAGE_SIZE) || !size.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::NotAligned);
        }
        let end = start + size;

        // 按起始地址排序检查重叠
        for r in &self.regions {
            if start < r.end && end > r.start {
                return Err(MapError::AlreadyMapped);
            }
        }

        let region = Region {
            start,
            end,
            flags,
            kind,
        };
        let idx = self.regions.partition_point(|r| r.start < start);
        self.regions.insert(idx, region);
        Ok(())
    }

    /// 查询虚拟地址所属的 Region。
    pub fn region_find(&self, vaddr: VirtAddr) -> Option<&Region> {
        let addr = vaddr.as_usize();
        let idx = self.regions.partition_point(|r| r.start <= addr);
        if idx == 0 {
            return None;
        }
        let region = &self.regions[idx - 1];
        if addr < region.end {
            Some(region)
        } else {
            None
        }
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        let alloc = crate::memory::allocator::page::allocator();
        // SAFETY: AddressSpace 独占根页表，drop 后不再使用。
        // 跳过来自内核根表的共享 L2 子树（DRAM identity/MMIO/高半区），
        // 只释放本空间私有的（任务栈、缺页映射的页表树）+ 根表本身。
        unsafe {
            self.root_mut().clean(&self.shared_l2, 2, alloc);
            PageTable::deallocate(self.root, alloc);
        }
    }
}

// ── 内核地址空间 ─────────────────────────────────────────────

/// 内核地址空间。`memory::init()` 创建并写入，此后只读访问。
///
/// 用 RelLock（可重入锁）：持有此锁期间若触发缺页，缺页处理器（trap.rs）
/// 会在同一 hart 上再次获取它——RelLock 允许同 hart 重入，避免自旋死锁；
/// 不同 hart 之间仍互斥。
static KERNEL_SPACE: RelLock<Option<AddressSpace>> = RelLock::new(None);

/// 获取内核地址空间的锁保护引用。
pub fn kernel_space() -> crate::lock::reentrant::RelLockGuard<'static, Option<AddressSpace>> {
    KERNEL_SPACE.lock()
}

/// 初始化 MMU：创建内核地址空间，identity-map DRAM 和 MMIO，启用 Sv39 分页。
///
/// 必须在 `memory::allocator::init()` 之后、在驱动程序 MMIO 访问之前调用。
///
/// # Safety
///
/// 写入 `satp` 后会立即启用分页。调用者需确保此时所有存活的指针
/// （栈、代码、数据段）都已 identity-mapped。
/// # Errors
///
/// - [`MapError::OutOfMemory`] — 物理帧不足以分配根页表或中间页表。
pub unsafe fn init() -> Result<(), MapError> { unsafe {
    use crate::hal::csr::satp;
    let alloc = crate::memory::allocator::page::allocator();
    let cfg = platform::get();

    // 任务栈窗口 TASK_STACK_BASE=0xC0000000 的前提：DRAM 必须 < 1 GiB。
    // 否则窗口落入 DRAM 恒等映射区，任务栈覆盖真实内存而非专用窗口。
    assert!(
        cfg.dram_size <= 0x4000_0000,
        "task stack window (TASK_STACK_BASE) requires DRAM < 1 GiB (got {:#x})",
        cfg.dram_size
    );

    // 1. 创建内核地址空间
    let mut kernel_space = AddressSpace::new(alloc)?;

    // 2. Identity-map DRAM
    let ram_flags = PteFlags::V
        | PteFlags::R
        | PteFlags::W
        | PteFlags::X
        | PteFlags::A
        | PteFlags::D
        | PteFlags::G;

    kernel_space.map(
        VirtAddr::from_raw(cfg.dram_base),
        PhysAddr::from_raw(cfg.dram_base),
        cfg.dram_size,
        ram_flags,
        alloc,
    )?;

    // 3. 建立内核高半区映射（为 S-mode 切换做准备）
    let kernel_va_base = VirtAddr::from_raw(VirtAddr::KERNEL_BASE + cfg.dram_base);
    kernel_space.map(
        kernel_va_base,
        PhysAddr::from_raw(cfg.dram_base),
        cfg.dram_size,
        ram_flags,
        alloc,
    )?;

    // 4. 启用 Sv39 分页
    let satp_val = satp::make(satp::MODE_SV39, 0, kernel_space.root_page());
    satp::write(satp_val);

    // 6. 刷新 TLB
    flush_tlb();

    // 7. 保存内核地址空间
    KERNEL_SPACE.lock().replace(kernel_space);

    Ok(())
}}
