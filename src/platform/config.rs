// 平台硬件参数
//
// 由 `platform::init()` 在引导早期从 DTB 提取，全局只读。
// 包含 DRAM、定时器频率等全局硬件属性；设备信息经 [`dtb`] 句柄供 device 阶段重解析。

use super::Dtb;
use crate::lock::OnceLock;

/// 平台硬件配置（只读，初始化后不可变）。
///
/// 只包含早期引导必需的全局属性。
#[derive(Debug)]
pub struct Config {
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

impl Config {
    /// 用 QEMU virt 硬编码默认值构造（DTB 缺失或解析失败时的回退）。
    fn default_qemu_virt() -> Self {
        Self {
            dram_base: super::qemu_virt::DRAM_BASE,
            dram_size: super::qemu_virt::DRAM_SIZE,
            timebase_frequency: super::qemu_virt::TIMEBASE_FREQUENCY,
            stack_reserve: 32 * 1024,
            hart_count: 1, // QEMU virt 默认单核
        }
    }
}

// ── 全局配置 ────────────────────────────────────────────────

/// 全局平台配置 — 引导早期写入一次，此后只读高频访问。
static PLATFORM: OnceLock<Config> = OnceLock::new();

/// 校验过的 DTB 句柄 — 供 `driver::device::probe()` 重解析设备。
static DTB: OnceLock<Dtb> = OnceLock::new();

/// 从 DTB 探测平台参数。
///
/// 引导早期（allocator 就绪前）调用，只解析全局信息：
/// - DRAM 基址和大小（供 MMU 用）
/// - 定时器频率（供 CLINT 用）
///
/// DTB 缺失或无效时回退 QEMU virt 默认值，错误即时输出（SBI writer，boot 即可用）。
/// 设备发现延迟到 [`driver::device::probe`]（allocator 就绪后），经 [`dtb`] 复用句柄。
///
/// # Safety
///
/// `ptr` 必须为 0 或指向有效的 FDT 数据；须在引导早期单 hart 下调用恰好一次。
pub unsafe fn init(ptr: usize) { unsafe {
    let cfg = match ptr {
        0 => Config::default_qemu_virt(),
        _ => match Dtb::new(ptr) {
            Ok(dtb) => {
                // 保存校验句柄，供 allocator 就绪后的设备发现重解析（仅成功时）
                let _ = DTB.set(dtb);
                probe_global(&dtb)
            }
            Err(_) => {
                crate::println!("[platform] DTB parse failed, using qemu-virt defaults");
                Config::default_qemu_virt()
            }
        },
    };
    let _ = PLATFORM.set(cfg);
}}

/// 获取平台配置的静态引用。
///
/// # Panics
///
/// 若 `init()` 未被调用。
#[inline]
pub fn get() -> &'static Config {
    PLATFORM.get().expect("platform::init() not called")
}

/// 读取校验过的 DTB 句柄（供 `driver::device::probe` 重解析）。
///
/// DTB 缺失或无效时为 `None`（设备发现走回退列表）。
pub(crate) fn dtb() -> Option<&'static Dtb> {
    DTB.get()
}

// ── DTB 探测（早阶段）────────────────────────────────────

/// 从 DTB 解析全局平台信息。
///
/// 单遍遍历 DTB：
/// - 识别 memory 节点获取 DRAM
/// - 识别 /cpus 节点获取 timebase 频率和 hart 数量
///
/// # Safety
///
/// `dtb` 必须是已校验的句柄（由 [`init`] 保证）。
fn probe_global(dtb: &Dtb) -> Config {
    let mut cfg = Config::default_qemu_virt();
    let mut cpu_count: usize = 0;

    // 单遍遍历 DTB：memory → DRAM，cpus → timebase + hart count
    for node in dtb.walk() {
        let name = node.name(dtb);

        if let Some(dev_type) = node.property_string(dtb, "device_type") {
            let dev_type = dev_type.trim_end_matches('\0');
            match dev_type {
                "memory" => {
                    if let Some((base, size)) = node.property_reg(dtb, 0) {
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

        if name.split('@').next() == Some("cpus")
            && let Some(freq) = node.property_u32(dtb, "timebase-frequency") {
                cfg.timebase_frequency = freq as u64;
            }
    }

    if cpu_count > 0 {
        cfg.hart_count = cpu_count;
    }

    cfg
}
