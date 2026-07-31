// 平台硬件参数
//
// 由 `platform::probe()` 在引导早期从 DTB 提取，全局只读。
// 包含 DRAM、定时器频率等全局硬件属性，设备信息通过发现缓冲区查询。

use super::Dtb;
use crate::lock::BareLock;

/// 平台硬件配置（只读，初始化后不可变）。
///
/// 只包含早期引导必需的全局属性。
/// 设备信息通过 [`DeviceNode`](super::discovery::DeviceNode) 查询。
#[derive(Debug)]
pub struct Platform {
    /// DRAM 物理基址
    pub dram_base: usize,
    /// DRAM 总大小 (bytes)
    pub dram_size: usize,
    /// 定时器频率 (Hz)
    pub timebase_frequency: u64,
    /// 内核栈保留大小（从 DRAM 末尾向下预留）。
    pub stack_reserve: usize,
    /// CPU / hart 数量。
    pub hart_count: usize,
}

impl Platform {
    /// 用 QEMU virt 硬编码默认值构造。
    fn default_qemu_virt() -> Self {
        Self {
            dram_base: super::qemu_virt::DRAM_BASE,
            dram_size: super::qemu_virt::DRAM_SIZE,
            timebase_frequency: super::qemu_virt::TIMEBASE_FREQ,
            stack_reserve: 32 * 1024,
            hart_count: 1, // QEMU virt 默认单核
        }
    }
}

// ── 全局配置 ────────────────────────────────────────────────

/// 全局平台配置 — 引导早期写入一次，此后只读。
static mut PLATFORM: Option<Platform> = None;

/// 保存的 DTB 物理地址 — 供 `driver::tree::probe()` 重解析用。
static mut SAVED_DTB: Option<usize> = None;

/// DTB 探测错误缓存 — probe 阶段填充，Phase 2 后输出。
/// 仅在引导期任务上下文访问，从不被中断处理程序碰，故用 BareLock。
static PROBE_ERROR: BareLock<Option<&'static str>> = BareLock::new(None);

/// 取出保存的 DTB 指针（供 driver::tree 调用）。
///
/// # Safety
///
/// 单 hart 引导期调用一次，与 `probe()` 无并发。
pub(crate) unsafe fn take_saved_dtb() -> Option<usize> {
    core::ptr::replace(core::ptr::addr_of_mut!(SAVED_DTB), None)
}

/// 从 DTB 探测平台参数。
///
/// 引导早期（allocator 就绪前）调用，只解析全局信息：
/// - DRAM 基址和大小（供 MMU 用）
/// - 定时器频率（供 CLINT 用）
///
/// 设备发现延迟到 [`driver::tree::probe`]（allocator 就绪后）。
///
/// # Safety
///
/// 必须在引导早期、单 hart 下调用恰好一次，在任何读取 `get()` 之前。
pub unsafe fn init(dtb_ptr: usize) {
    let cfg = if dtb_ptr != 0 {
        probe_dtb_global(dtb_ptr)
    } else {
        Platform::default_qemu_virt()
    };

    // 保存 DTB 指针，供 allocator 就绪后的设备发现重解析
    if dtb_ptr != 0 {
        core::ptr::write(core::ptr::addr_of_mut!(SAVED_DTB), Some(dtb_ptr));
    }

    PLATFORM = Some(cfg);
}

/// 获取平台配置的静态引用。
///
/// # Panics
///
/// 若 `probe()` 未被调用。
#[inline]
pub fn get() -> &'static Platform {
    // SAFETY: 引导早期单 hart 写入，此后只读；中断使能前已写入完毕。
    // 使用 addr_of! 避免 static_mut_refs 警告。
    unsafe {
        (*core::ptr::addr_of!(PLATFORM))
            .as_ref()
            .expect("platform::probe() not called")
    }
}

/// 输出缓存的 DTB 探测错误（在日志初始化后调用）。
pub fn report_probe_error() {
    // SAFETY: 仅引导期任务上下文调用，不会从中断上下文争用 PROBE_ERROR。
    let mut err = unsafe { PROBE_ERROR.lock() };
    if let Some(msg) = *err {
        crate::warn!("DTB parse failed, using fallback: {}", msg);
        *err = None;
    }
}

// ── DTB 探测（早阶段）────────────────────────────────────

/// 从 DTB 解析全局平台信息。
///
/// 单遍遍历 DTB：
/// - 识别 memory 节点获取 DRAM
/// - 识别 /cpus 节点获取 timebase 频率和 hart 数量
/// - 每个有 `compatible` + `reg` 的节点自动推入发现缓冲区
///
/// # Safety
///
/// `dtb_ptr` 必须指向有效的 FDT 头部。
unsafe fn probe_dtb_global(dtb_ptr: usize) -> Platform {
    let dtb = match Dtb::new(dtb_ptr) {
        Ok(d) => d,
        Err(_) => {
            PROBE_ERROR.lock().replace("invalid DTB header");
            return Platform::default_qemu_virt();
        }
    };

    let mut cfg = Platform::default_qemu_virt();
    let mut cpu_count: usize = 0;

    // 单遍遍历 DTB：memory → DRAM，cpus → timebase + hart count，设备 → 发现缓冲区
    // SAFETY: DTB 物理内存始终有效。
    for node in dtb.walk() {
        let name = node.name(&dtb);

        if let Some(dev_type) = node.property_string(&dtb, "device_type") {
            let dev_type = dev_type.trim_end_matches('\0');
            match dev_type {
                "memory" => {
                    if let Some((base, size)) = node.property_reg(&dtb, 0) {
                        cfg.dram_base = base as usize;
                        cfg.dram_size = size as usize;
                    }
                }
                "cpu" => {
                    cpu_count += 1;
                }
                _ => {}
            }
        }

        if name.split('@').next() == Some("cpus") {
            if let Some(freq) = node.property_u32(&dtb, "timebase-frequency") {
                cfg.timebase_frequency = freq as u64;
            }
        }
    }

    if cpu_count > 0 {
        cfg.hart_count = cpu_count;
    }

    cfg
}
