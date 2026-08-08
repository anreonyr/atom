// loader/load.rs — 段装载（组合层）
//
// 把 Elf64 解析结果逐段映射进新地址空间：PT_LOAD → 代码 R|X、数据 R|W、
// .bss 零页（memsz > filesz 填零）。复用 map_user_code 的 U 页映射路径
// （frame 取帧 → PA 直写 → space.map + page::allocator），逐段 flags。
// 返回 (Box<AddressSpace>, entry_va)，供 spawn 侧 TaskBuilder::loader 消费。
// 不依赖 schedule（无环）；依赖 memory + 本模块 elf。

use alloc::boxed::Box;
use core::alloc::Layout;

use crate::memory::{
    MapError, PAGE_SIZE,
    addr::{PhysAddr, VirtAddr},
    allocator::{frame, page},
    entry::PteFlags,
    space::AddressSpace,
};

use super::elf::{Elf64, ElfError, PF_W, PF_X, Segment};

/// 装载错误 — 变体即行为：Elf 格式非法不 spawn；Map 页表帧耗尽/段重叠；
/// OutOfMemory 数据帧耗尽。
#[derive(Debug)]
#[allow(dead_code)] // payload 为错误语义上下文，仅在 Debug 输出时读取（dead_code 不识别）
pub enum LoadError {
    /// ELF 解析/校验失败（调用方：日志 + 不 spawn）
    Elf(ElfError),
    /// 页表操作失败（from_kernel / space.map；页表帧耗尽 / 段重叠 AlreadyMapped）
    Map(MapError),
    /// 数据物理帧耗尽
    OutOfMemory,
}

/// 从 ELF blob 装载用户程序：解析 + 建新地址空间 + 逐段映射。
///
/// 返回 `(space, entry)`：`space` 已含全部 PT_LOAD 段映射（用户半区，逐段
/// flags），`entry` 为 ELF 入口虚拟地址。任务栈由 spawn 侧统一映射（固定
/// 窗口），loader 不管。
///
/// # Errors
///
/// 见 [`LoadError`]。装载中途失败时部分映射的地址空间随 `Box` drop 释放
/// 页表树；已分配的数据帧与 map_user_code 现状一致随空间泄漏（已知限制）。
pub fn load(blob: &[u8]) -> Result<(Box<AddressSpace>, VirtAddr), LoadError> {
    let elf = Elf64::parse(blob).map_err(LoadError::Elf)?;
    let alloc = page::allocator();
    let mut space = Box::new(AddressSpace::from_kernel(alloc).map_err(LoadError::Map)?);

    for i in 0..elf.phnum() {
        if let Some(seg) = elf.phdr(i) {
            load_segment(&mut space, seg, blob)?;
        }
    }

    Ok((space, VirtAddr::from_raw(elf.entry())))
}

/// 向下页对齐。
fn align_down(x: usize) -> usize {
    x & !(PAGE_SIZE - 1)
}

/// 向上页对齐；溢出（越界 usize 上界）返回 `None`。
fn align_up(x: usize) -> Option<usize> {
    x.checked_add(PAGE_SIZE - 1).map(|v| v & !(PAGE_SIZE - 1))
}

/// 装载单个 PT_LOAD 段：逐页分配帧 → 清零 → 拷文件字节 → 按权限映射。
///
/// 页范围 [align_down(vaddr), align_up(vaddr + memsz))。每页：
/// 1. frame 取物理帧（frame 分配器不保证清零）；
/// 2. 显式清零整页——覆盖 .bss 与页首/页尾未对齐区；
/// 3. 拷入该页与文件字节的交叠区间 `[max(vaddr, va), min(vaddr+filesz, va+4096))`；
/// 4. 按段权限推导 flags 映射（R|U|V|A 恒置；PF_W → W|D；PF_X → X）。
fn load_segment(space: &mut AddressSpace, seg: Segment, blob: &[u8]) -> Result<(), LoadError> {
    // parse 已保证 vaddr+memsz 不溢出；此处补 align_up 上界防护
    let page_start = align_down(seg.vaddr);
    let page_end = align_up(seg.vaddr + seg.memsz).ok_or(LoadError::Elf(
        ElfError::InvalidSegment("p_vaddr + p_memsz too large"),
    ))?;
    let mut flags = PteFlags::V | PteFlags::R | PteFlags::U | PteFlags::A;
    if seg.flags & PF_W != 0 {
        flags |= PteFlags::W | PteFlags::D;
    }
    if seg.flags & PF_X != 0 {
        flags |= PteFlags::X;
    }
    let alloc = page::allocator();

    for va in (page_start..page_end).step_by(PAGE_SIZE) {
        let frame = frame::allocator()
            .allocate(Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap())
            .map_err(|_| LoadError::OutOfMemory)?;
        let pa = frame.as_ptr() as *mut u8 as usize;
        // SAFETY: 物理帧刚分配（页对齐、长度 ≥ 一页），DRAM 恒等映射区可直写。
        unsafe {
            core::ptr::write_bytes(pa as *mut u8, 0u8, PAGE_SIZE);
        }
        // 文件字节与该页的交叠区间；.bss（memsz > filesz）自然落空 → 保持零
        let copy_start = seg.vaddr.max(va);
        let copy_end = (seg.vaddr + seg.filesz).min(va + PAGE_SIZE);
        if copy_start < copy_end {
            // SAFETY: parse 已保证 offset + filesz 在 blob 内，且 copy_end
            // ≤ vaddr + filesz，故 src 读取区间在 blob 内；dst 在本帧内。
            let src = unsafe { blob.as_ptr().add(seg.offset + (copy_start - seg.vaddr)) };
            let dst = pa + (copy_start - va);
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst as *mut u8, copy_end - copy_start);
            }
        }
        space
            .map(
                VirtAddr::from_raw(va),
                PhysAddr::from_raw(pa),
                PAGE_SIZE,
                flags,
                alloc,
            )
            .map_err(LoadError::Map)?;
    }
    Ok(())
}
