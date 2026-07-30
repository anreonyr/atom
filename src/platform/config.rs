// 平台硬件配置
//
// 由 `platform::init()` 在引导早期设置，全局只读。
// 包含 DRAM、定时器频率等全局硬件属性，设备信息通过发现缓冲区查询。

use crate::lock::BareLock;
use super::{discovery, Dtb};

/// 平台硬件配置（只读，初始化后不可变）。
///
/// 只包含早期引导必需的全局属性。
/// 设备信息通过 [`DeviceNode`](super::discovery::DeviceNode) 查询。
#[derive(Debug)]
pub struct PlatformConfig {
    /// DRAM 物理基址
    pub dram_base: usize,
    /// DRAM 总大小 (bytes)
    pub dram_size: usize,
    /// 定时器频率 (Hz)
    pub timebase_freq: u64,
    /// 固件保留的 DRAM 起始大小 — 由 `_kernel_start - dram_base` 运行时推导。
    pub firmware_reserve: usize,
    /// 内核栈保留大小（从 DRAM 末尾向下预留）。
    pub stack_reserve: usize,
    /// CPU / hart 数量。
    pub hart_count: usize,
}

impl PlatformConfig {
    /// 用 QEMU virt 硬编码默认值构造。
    fn default_qemu_virt() -> Self {
        Self {
            dram_base: super::qemu_virt::DRAM_BASE,
            dram_size: super::qemu_virt::DRAM_SIZE,
            timebase_freq: super::qemu_virt::TIMEBASE_FREQ,
            firmware_reserve: 0,  // 由 init() 中链接符号推导覆盖
            stack_reserve: 32 * 1024,
            hart_count: 1,  // QEMU virt 默认单核
        }
    }
}

// ── 全局配置 ────────────────────────────────────────────────

/// 全局平台配置 — 引导早期写入一次，此后只读。
static mut PLATFORM: Option<PlatformConfig> = None;

/// DTB 解析诊断信息缓存 — probe 阶段填充，Phase 2 后输出。
/// 仅在引导期任务上下文访问，从不被中断处理程序碰，故用 BareLock。
static DTB_DIAG: BareLock<Option<&'static str>> = BareLock::new(None);

/// 探测并初始化平台配置。
///
/// 引导早期（allocator 就绪前）调用，只解析全局信息：
/// - DRAM 基址和大小（供 MMU 用）
/// - 定时器频率（供 CLINT 用）
///
/// 设备发现延迟到 [`discovery::discover`]（allocator 就绪后）。
///
/// # Safety
///
/// 必须在引导早期、单 hart 下调用恰好一次，在任何读取 `config()` 之前。
pub unsafe fn init(dtb_ptr: usize) {
    let mut cfg = if dtb_ptr != 0 {
        let probed = probe_dtb_global(dtb_ptr);
        *DTB_DIAG.lock() = None;
        probed
    } else {
        PlatformConfig::default_qemu_virt()
    };

    // 从链接符号推导固件保留大小（DRAM_BASE 到 _kernel_start 之间）
    extern "C" {
        static _kernel_start: u8;
    }
    cfg.firmware_reserve = &raw const _kernel_start as usize - cfg.dram_base;

    PLATFORM = Some(cfg);
}

/// 获取平台配置的静态引用。
///
/// # Panics
///
/// 若 `init()` 未被调用。
#[inline]
pub fn config() -> &'static PlatformConfig {
    // SAFETY: 引导早期单 hart 写入，此后只读；中断使能前已写入完毕。
    // 使用 addr_of! 避免 static_mut_refs 警告。
    unsafe {
        (*core::ptr::addr_of!(PLATFORM))
            .as_ref()
            .expect("platform::init() not called")
    }
}

/// 输出缓存的 DTB 诊断信息（在日志初始化后调用）。
pub fn report_diag() {
    // SAFETY: 仅引导期任务上下文调用，不会从中断上下文争用 DTB_DIAG。
    let mut diag = unsafe { DTB_DIAG.lock() };
    if let Some(msg) = *diag {
        crate::warn!("DTB parse failed, using fallback: {}", msg);
        *diag = None;
    }
}

// ── DTB 探测（早阶段）────────────────────────────────────

/// 从 DTB 解析全局平台信息和设备列表。
///
/// 单遍遍历 DTB：
/// - 识别 memory 节点获取 DRAM
/// - 识别 /cpus 节点获取 timebase 频率
/// - 每个有 `compatible` + `reg` 的节点自动推入发现缓冲区
///
/// # Safety
///
/// `dtb_ptr` 必须指向有效的 FDT 头部。
unsafe fn probe_dtb_global(dtb_ptr: usize) -> PlatformConfig {
    let dtb = match Dtb::new(dtb_ptr) {
        Ok(d) => d,
        Err(_) => return PlatformConfig::default_qemu_virt(),
    };

    let mut cfg = PlatformConfig::default_qemu_virt();

    // 单遍遍历 DTB：memory → DRAM，cpus → timebase，设备 → 发现缓冲区
    // SAFETY: DTB 物理内存始终有效。
    for node in dtb.walk() {
        let name = node.name(&dtb);

        if let Some(dev_type) = node.property_string(&dtb, "device_type") {
            if dev_type.trim_end_matches('\0') == "memory" {
                if let Some((base, size)) = node.property_reg(&dtb, 0) {
                    cfg.dram_base = base as usize;
                    cfg.dram_size = size as usize;
                }
            }
        }

        if name.split('@').next() == Some("cpus") {
            if let Some(freq) = node.property_u32(&dtb, "timebase-frequency") {
                cfg.timebase_freq = freq as u64;
            }
        }

        // 设备发现：有 compatible + reg(size>0) 的设备推入缓冲区
        if let Some((base, size)) = node.property_reg(&dtb, 0) {
            if size > 0 {
                if let Some(compatible) = node.property_string(&dtb, "compatible") {
                    let compatible: &'static str = unsafe { core::mem::transmute(compatible) };
                    let interrupt = node.property_u32(&dtb, "interrupts");
                    discovery::push_early(compatible, base as usize, size as usize, interrupt);
                }
            }
        }
    }

    cfg
}
