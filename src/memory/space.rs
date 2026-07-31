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
/// 单 hart 下，`map`/`unmap`/`protect` 通过 `&self` + 内部裸指针操作：
/// 初始化期间无并发；运行时中断禁用，trap handler 内的 `translate` 与
/// 内核路径的操作不会交错。
///
/// # Drop
///
/// 释放时递归回收所有页表帧（根表 + 中间表）到页分配器。
pub struct AddressSpace {
    root: NonNull<PageTable>,
    regions: Vec<Region>,
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
        let space = Self::new(alloc)?;
        let guard = KERNEL_SPACE.lock();
        if let Some(ref ks) = *guard {
            let src = unsafe { ks.root_ref() };
            let dst = unsafe { space.root_mut() };
            dst.entries.copy_from_slice(&src.entries);
        }
        Ok(space)
    }

    // ── 映射操作 ──────────────────────────────────────────────

    /// 映射 `size` 字节虚拟地址到物理地址。
    ///
    /// 纯页表操作：仅安装 PTE，不注册 Region。按需分配中间页表。
    ///
    /// # Errors
    ///
    /// 参见 [`PageTable::map`]。
    pub fn map(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        size: usize,
        flags: PteFlags,
        alloc: &dyn Allocator,
    ) -> Result<(), MapError> {
        // SAFETY: 地址空间已初始化，map 只修改页表
        unsafe { self.root_mut().map(vaddr, paddr, size, flags, alloc) }
    }

    /// 取消映射一段虚拟地址。
    ///
    /// 不释放中间页表（惰性策略）。Region 管理由调用方负责。
    pub fn unmap(&self, vaddr: VirtAddr, size: usize) {
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            // SAFETY: 地址空间已初始化，unmap 只清零叶子 PTE
            unsafe { self.root_mut().unmap(vaddr + i * PAGE_SIZE) };
        }
    }

    /// 修改已映射区域的保护标志。
    ///
    /// 单次遍历，不分配中间表——叶子 PTE 不存在则返回错误。
    ///
    /// # Errors
    ///
    /// 任一页的叶子 PTE 不存在时返回 [`MapError::NotMapped`]。
    pub fn protect(&self, vaddr: VirtAddr, size: usize, flags: PteFlags) -> Result<(), MapError> {
        let pages = size.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            // SAFETY: 地址空间已初始化，protect 只修改已有叶子 PTE 标志位
            let leaf = unsafe { self.root_mut().walk_mut(va, None)? };
            leaf.set_flags(flags | PteFlags::V);
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
        &self,
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
        // SAFETY: 地址空间已初始化，只读遍历
        unsafe { self.root_ref().walk_ref(vaddr) }
    }

    /// 返回根页表页号（写入 `satp` 用）。
    pub fn root_page(&self) -> u64 {
        (self.root.as_ptr() as usize >> crate::memory::PAGE_SHIFT) as u64
    }

    // ── 地址空间共享 ──────────────────────────────────────────

    /// 从另一个地址空间复制内核半区（L2 条目 256–511）。
    ///
    /// 用于创建用户地址空间时共享内核映射。
    pub fn share_kernel(&mut self, kernel: &AddressSpace) {
        // SAFETY: 内核地址空间已初始化
        let src = unsafe { kernel.root_ref() };
        let dst = unsafe { self.root_mut() };
        dst.entries[256..512].copy_from_slice(&src.entries[256..512]);
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

    /// 删除与 `[start, start+size)` 重叠的所有 Region。
    pub fn region_remove(&mut self, start: usize, size: usize) {
        let end = start + size;
        self.regions.retain(|r| !(start < r.end && end > r.start));
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
        // SAFETY: AddressSpace 独占根页表，drop 后不再使用
        unsafe {
            self.root_mut().clean(2, alloc);
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

/// 用户进程活动地址空间。调度器在切换到用户任务时设置此值，
/// 缺页处理器通过 `active_space()` 获取而非硬编码 `kernel_space()`。
static ACTIVE_SPACE: RelLock<Option<&'static AddressSpace>> = RelLock::new(None);

/// 获取活动地址空间的锁保护引用（用于缺页处理）。
pub fn active_space() -> crate::lock::reentrant::RelLockGuard<'static, Option<&'static AddressSpace>>
{
    ACTIVE_SPACE.lock()
}

/// 初始化 MMU：创建内核地址空间，identity-map DRAM 和 MMIO，启用 Sv39 分页。
///
/// 必须在 `memory::allocator::init()` 之后、在驱动程序 MMIO 访问之前调用。
///
/// # Safety
///
/// 写入 `satp` 后会立即启用分页。调用者需确保此时所有存活的指针
/// （栈、代码、数据段）都已 identity-mapped。
pub unsafe fn init() {
    use crate::hal::csr::satp;
    let alloc = crate::memory::allocator::page::allocator();
    let cfg = platform::config();

    // 1. 创建内核地址空间
    let kernel_space =
        AddressSpace::new(alloc).expect("memory: failed to create kernel address space");

    // 2. Identity-map DRAM
    let ram_flags = PteFlags::V
        | PteFlags::R
        | PteFlags::W
        | PteFlags::X
        | PteFlags::A
        | PteFlags::D
        | PteFlags::G;

    kernel_space
        .map(
            VirtAddr::new_truncate(cfg.dram_base),
            PhysAddr::from_raw(cfg.dram_base),
            cfg.dram_size,
            ram_flags,
            alloc,
        )
        .expect("memory: failed to identity-map DRAM");

    // 3. Identity-map MMIO 设备（无 X 位，不可执行）
    let dev_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D | PteFlags::G;

    crate::drivers::for_each(|dev| {
        let size = if dev.size > 0 { dev.size } else { 0x1000 };
        kernel_space
            .map(
                VirtAddr::new_truncate(dev.base),
                PhysAddr::from_raw(dev.base),
                size,
                dev_flags,
                alloc,
            )
            .expect("memory: failed to map MMIO device");
    });

    // 4. 建立内核高半区映射（为 S-mode 切换做准备）
    let kernel_va_base = VirtAddr::new_truncate(VirtAddr::KERNEL_BASE + cfg.dram_base);
    kernel_space
        .map(
            kernel_va_base,
            PhysAddr::from_raw(cfg.dram_base),
            cfg.dram_size,
            ram_flags,
            alloc,
        )
        .expect("memory: failed to map kernel high-half");

    // 5. 启用 Sv39 分页
    let satp_val = satp::make(satp::MODE_SV39, 0, kernel_space.root_page() as usize);
    satp::write(satp_val);

    // 6. 刷新 TLB
    flush_tlb();

    // 7. 保存内核地址空间
    KERNEL_SPACE.lock().replace(kernel_space);
}


