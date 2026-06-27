// Sv39 页表项 (Page Table Entry) — 64-bit PTE 类型定义与操作
//
// Sv39 三级页表的每个条目为 8 字节，位布局：
//   0:9   — 标志位 (V, R, W, X, U, G, A, D, RSW*2)
//   10:53 — PPN (物理页号, 44 bits, 对应 56-bit 物理地址的 12:55)
//   54:63 — 保留 (必须为零)

#![allow(dead_code)]

use core::fmt;

/// Sv39 页表项
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PageTableEntry {
    bits: u64,
}

// #[repr(transparent)]
// #[derive(Clone, Copy)]
// type PageTableEntry = u64;

impl PageTableEntry {
    // ── 标志位 ────────────────────────────────────────────────
    /// Valid — PTE 有效
    pub const V: u64 = 1 << 0;
    /// Read — 可读
    pub const R: u64 = 1 << 1;
    /// Write — 可写
    pub const W: u64 = 1 << 2;
    /// Execute — 可执行
    pub const X: u64 = 1 << 3;
    /// User — 用户态可访问
    pub const U: u64 = 1 << 4;
    /// Global — 全局映射（不随 sfence.vma ASID 刷新）
    pub const G: u64 = 1 << 5;
    /// Accessed — 已被访问（硬件会置位）
    pub const A: u64 = 1 << 6;
    /// Dirty — 已被写入（硬件会置位）
    pub const D: u64 = 1 << 7;

    const FLAGS_MASK: u64 = 0x3FF; // bits 0-9
    const PPN_SHIFT: u64 = 10;
    const PPN_MASK: u64 = 0xFFFF_FFFF_FFC0; // (0x000F_FFFF_FFFF << 10) in u64

    /// 创建一个无效的空 PTE（全零）
    #[inline(always)]
    pub const fn empty() -> Self {
        PageTableEntry { bits: 0 }
    }

    /// 从物理页号和标志位构造 PTE
    ///
    /// # Safety
    ///
    /// 调用者需确保 `ppn` 不超过 44 位（物理地址右移 12 位后）。
    #[inline(always)]
    pub const fn new(ppn: u64, flags: u64) -> Self {
        PageTableEntry {
            bits: (ppn << Self::PPN_SHIFT) | (flags & Self::FLAGS_MASK),
        }
    }

    /// PTE 是否有效（V=1）
    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.bits & Self::V != 0
    }

    /// 是否为叶子节点（R|W|X 中任一位被设置）
    ///
    /// 非叶子节点允许只设 V 位，R/W/X 全为零。
    #[inline(always)]
    pub fn is_leaf(&self) -> bool {
        self.bits & (Self::R | Self::W | Self::X) != 0
    }

    /// 提取 PPN（44 位，已右移 12 位对齐 — 直接左移 12 得物理地址）
    #[inline(always)]
    pub fn ppn(&self) -> u64 {
        self.bits >> Self::PPN_SHIFT
    }

    /// 提取物理地址（PPN << 12）
    #[inline(always)]
    pub fn paddr(&self) -> u64 {
        self.ppn() << 12
    }

    /// 提取标志位 (bits 0-9)
    #[inline(always)]
    pub fn flags(&self) -> u64 {
        self.bits & Self::FLAGS_MASK
    }

    /// 设置 PPN 和标志位
    #[inline(always)]
    pub fn set(&mut self, ppn: u64, flags: u64) {
        self.bits = (ppn << Self::PPN_SHIFT) | (flags & Self::FLAGS_MASK);
    }

    /// 清除 PTE（设为无效）
    #[inline(always)]
    pub fn clear(&mut self) {
        self.bits = 0;
    }

    /// 获取原始 u64 值
    #[inline(always)]
    pub fn as_u64(&self) -> u64 {
        self.bits
    }
}

impl fmt::Debug for PageTableEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_valid() {
            write!(f, "PTE(invalid)")
        } else {
            write!(f, "PTE({:#x}, ppn={:#x}, flags=", self.bits, self.ppn())?;
            if self.bits & Self::R != 0 {
                write!(f, "R")?;
            }
            if self.bits & Self::W != 0 {
                write!(f, "W")?;
            }
            if self.bits & Self::X != 0 {
                write!(f, "X")?;
            }
            if self.bits & Self::U != 0 {
                write!(f, "U")?;
            }
            if self.bits & Self::G != 0 {
                write!(f, "G")?;
            }
            if self.bits & Self::A != 0 {
                write!(f, "A")?;
            }
            if self.bits & Self::D != 0 {
                write!(f, "D")?;
            }
            write!(f, ")")
        }
    }
}

impl core::default::Default for PageTableEntry {
    fn default() -> Self {
        Self::empty()
    }
}
