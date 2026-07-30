// DTB 设备发现 — allocator 就绪后调用
//
// `platform::init()` 在引导早期（allocator 前）解析 DTB，将发现的设备
// 存入固定缓冲区。`discover()` 在 allocator 就绪后将缓冲区转成 Vec。

use alloc::vec::Vec;

use crate::lock::OnceLock;

/// DTB 中发现的设备节点。
#[derive(Debug, Clone, Copy)]
pub struct DeviceNode {
    /// 匹配的 compatible 字符串
    pub compatible: &'static str,
    /// MMIO 基址 (reg[0])
    pub base: usize,
    /// MMIO 区域大小 (reg[0])
    pub size: usize,
    /// 中断号（如有）
    pub interrupt: Option<u32>,
}

// ── 早期缓冲区（no allocator）─────────────────────────────

/// 引导早期 DTB 解析时填充的固定设备缓冲区。
const MAX_EARLY: usize = 8;
pub(crate) static mut EARLY_BUF: [Option<DeviceNode>; MAX_EARLY] = [None; MAX_EARLY];
pub(crate) static mut EARLY_COUNT: usize = 0;

/// 向早期缓冲区添加设备（`platform::init()` 调用，单 hart 安全）。
pub(crate) unsafe fn push_early(compatible: &'static str, base: usize, size: usize, interrupt: Option<u32>) {
    if EARLY_COUNT < MAX_EARLY {
        EARLY_BUF[EARLY_COUNT] = Some(DeviceNode { compatible, base, size, interrupt });
        EARLY_COUNT += 1;
    }
}

// ── 运行期查询 ───────────────────────────────────────────

/// 设备发现结果 — `discover()` 填充一次，此后只读查询。
static DEVICES: OnceLock<Vec<DeviceNode>> = OnceLock::new();

/// 执行设备发现：将早期缓冲区转成 Vec，或使用回退。
///
/// 必须在 `allocator::init()` 之后、任何 `for_each()` 调用之前调用一次。
pub fn discover() {
    let devices = unsafe { take_early() }
        .unwrap_or_else(fallback_devices);
    let _ = DEVICES.set(devices);
}

/// 从早期缓冲区取出设备（若为空则返回 None）。
unsafe fn take_early() -> Option<Vec<DeviceNode>> {
    if EARLY_COUNT == 0 {
        return None;
    }
    let mut devices = Vec::with_capacity(EARLY_COUNT);
    for i in 0..EARLY_COUNT {
        if let Some(d) = EARLY_BUF[i].take() {
            devices.push(d);
        }
    }
    EARLY_COUNT = 0;
    Some(devices)
}

/// 遍历所有已发现设备。
pub fn for_each(mut f: impl FnMut(&DeviceNode)) {
    if let Some(vec) = DEVICES.get() {
        for dev in vec {
            f(dev);
        }
    }
}

// ── 回退 ─────────────────────────────────────────────────

/// DTB 不可用时的回退设备列表。
fn fallback_devices() -> Vec<DeviceNode> {
    Vec::from([
        DeviceNode {
            compatible: "riscv,plic0",
            base: crate::platform::qemu_virt::PLIC_BASE,
            size: crate::platform::qemu_virt::PLIC_SIZE,
            interrupt: None,
        },
        DeviceNode {
            compatible: "ns16550a",
            base: crate::platform::qemu_virt::UART_BASE,
            size: crate::platform::qemu_virt::UART_SIZE,
            interrupt: Some(crate::platform::qemu_virt::UART_INTERRUPT),
        },
        DeviceNode {
            compatible: "riscv,clint0",
            base: crate::platform::qemu_virt::CLINT_BASE,
            size: crate::platform::qemu_virt::CLINT_SIZE,
            interrupt: None,
        },
    ])
}
