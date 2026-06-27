// 缺页异常处理
//
// 替换 trap.rs 中的 panic，提供结构化的缺页诊断和处理框架。
// 当前阶段：内核缺页 fatal（打印诊断→panic），用户缺页 fatal（预留懒分配扩展点）。

use crate::hal::csr::{mepc, mtval};
use crate::hal::csr::mcause;
use crate::mmu::addr::VirtAddr;
use crate::mmu::entry::PteFlags;
use crate::mmu::space::AddressSpace;

// ── PageFault ────────────────────────────────────────────────────

/// 从机器 CSR 捕获的缺页信息。
#[derive(Debug)]
pub struct PageFault {
    /// 引发缺页的虚拟地址（来自 mtval）
    pub addr: VirtAddr,
    /// 缺页时的程序计数器（来自 mepc）
    pub pc: usize,
    /// 缺页类型
    pub kind: FaultKind,
}

/// 缺页类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// 指令缺页 (mcause = 12)
    Instruction,
    /// 加载缺页 (mcause = 13)
    Load,
    /// 存储/AMO 缺页 (mcause = 15)
    Store,
}

impl PageFault {
    /// 从当前 CSR 状态捕获缺页信息。
    ///
    /// 仅在 trap handler 内调用。
    pub unsafe fn capture() -> Self {
        let code = mcause::read().code();
        let kind = match code {
            12 => FaultKind::Instruction,
            13 => FaultKind::Load,
            15 => FaultKind::Store,
            _ => unreachable!("capture() called on non-page-fault mcause={}", code),
        };

        Self {
            addr: VirtAddr::new_truncate(mtval::read()),
            pc: mepc::read(),
            kind,
        }
    }
}

// ── Fault handler ────────────────────────────────────────────────

/// 处理缺页异常。
///
/// 返回 `true` 表示已解决（可以 mret 重试），`false` 表示无法处理。
///
/// # 处理策略
///
/// 1. Re-walk 页表 — 排除 A/D 位竞争（QEMU 硬件管理 A/D，通常不会触发）
/// 2. 权限违规 — 映射存在但权限不足 → fatal
/// 3. 真正缺页：
///    - 内核地址 → fatal（内核页必须预映射）
///    - 用户地址 → fatal（未来在此插入懒分配 / COW 逻辑）
pub fn handle_page_fault(fault: &PageFault, space: &AddressSpace) -> bool {
    // 1. Re-walk 页表 (A/D 位竞争检查)
    if let Some((_paddr, flags)) = space.translate(fault.addr) {
        if flags.contains(PteFlags::V) {
            // 映射存在 — 可能是 A/D 位的瞬时竞争，直接重试
            info!(
                "page fault resolved by re-walk: {:?} at {:?}",
                fault.kind, fault.addr
            );
            return true;
        }
    }

    // 2. 无法处理
    error!(
        "unhandled page fault: {:?} at {:?}, pc={:#x}",
        fault.kind, fault.addr, fault.pc
    );

    if fault.addr.is_kernel() {
        error!("kernel page fault — this is a bug (kernel pages must be pre-mapped)");
    } else {
        error!("user page fault — lazy allocation not yet implemented");
    }

    false
}
