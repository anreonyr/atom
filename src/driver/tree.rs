// 设备发现 — allocator 就绪后遍历 DTB，直接分配 Vec
//
// `platform::init()` 在引导早期（allocator 前）保存 DTB 指针。
// 本模块在 allocator 就绪后重新解析 DTB，将发现的设备存入 Vec。

use alloc::vec::Vec;

use crate::memory::addr::PhysAddr;
use crate::lock::OnceLock;
use crate::platform::Dtb;

/// DTB 中发现的设备节点。
#[derive(Debug, Clone, Copy)]
pub struct DeviceNode {
    /// 匹配的 compatible 字符串
    pub compatible: &'static str,
    /// MMIO 基址 (reg[0])
    pub base: PhysAddr,
    /// MMIO 区域大小 (reg[0])
    pub size: usize,
    /// 中断号（如有）
    pub interrupt: Option<u32>,
}

/// 设备发现结果 — `discover()` 填充一次，此后只读查询。
static DEVICES: OnceLock<Vec<DeviceNode>> = OnceLock::new();

/// 执行设备发现：从 DTB 解析设备列表，失败时使用回退。
///
/// 必须在 `allocator::init()` 之后、任何 `for_each()` 调用之前调用一次。
pub fn probe() {
    let devices = unsafe {
        match crate::platform::config::take_saved_dtb() {
            Some(ptr) => probe_devices(ptr),
            None => fallback_devices(),
        }
    };
    if DEVICES.set(devices).is_err() {
        panic!("devices already probed — probe() called more than once, this is a logic error");
    }
}

/// 遍历所有已发现设备。
pub fn for_each(mut f: impl FnMut(&DeviceNode)) {
    if let Some(vec) = DEVICES.get() {
        for dev in vec {
            f(dev);
        }
    }
}

// ── DTB 重解析 ────────────────────────────────────────────

/// 从 DTB 物理地址解析设备列表。
unsafe fn probe_devices(dtb_ptr: usize) -> Vec<DeviceNode> {
    let dtb = match Dtb::new(dtb_ptr) {
        Ok(d) => d,
        Err(_) => return fallback_devices(),
    };

    let mut devices = Vec::new();
    for node in dtb.walk() {
        if let Some((base, size)) = node.property_reg(&dtb, 0) {
            if size > 0 {
                if let Some(compatible) = node.property_string(&dtb, "compatible") {
                    // SAFETY: DTB physical memory is reserved by OpenSBI and never freed;
                    // the &str reference into it remains valid for the entire kernel lifetime.
                    let compatible: &'static str = core::mem::transmute(compatible);
                    let interrupt = node.property_u32(&dtb, "interrupts");
                    devices.push(DeviceNode {
                        compatible,
                        base: PhysAddr::from_raw(base as usize),
                        size: size as usize,
                        interrupt,
                    });
                }
            }
        }
    }
    devices
}

// ── 回退 ─────────────────────────────────────────────────

/// DTB 不可用时的回退设备列表。
fn fallback_devices() -> Vec<DeviceNode> {
    Vec::from([
        DeviceNode {
            compatible: "sifive,plic-1.0.0",
            base: PhysAddr::from_raw(crate::platform::qemu_virt::PLIC_BASE),
            size: crate::platform::qemu_virt::PLIC_SIZE,
            interrupt: None,
        },
        DeviceNode {
            compatible: "ns16550a",
            base: PhysAddr::from_raw(crate::platform::qemu_virt::UART_BASE),
            size: crate::platform::qemu_virt::UART_SIZE,
            interrupt: Some(crate::platform::qemu_virt::UART_INTERRUPT),
        },
        DeviceNode {
            compatible: "sifive,clint0",
            base: PhysAddr::from_raw(crate::platform::qemu_virt::CLINT_BASE),
            size: crate::platform::qemu_virt::CLINT_SIZE,
            interrupt: None,
        },
    ])
}
