pub mod clint;
pub mod hub;
pub mod plic;
pub mod traits;
pub mod tree;
pub mod uart;

// probe 是唯一直供外部调用的发现入口
pub use traits::{Driver, DriverError};
pub use tree::probe;

use crate::hal;
use crate::memory::{addr::VirtAddr, allocator::page, entry::PteFlags, space};

/// 完整的驱动初始化：DTB 探测 → MMIO 映射 → 创建实例 → hub 注册 → Driver::init() → 中断控制器注册。
///
/// 按依赖顺序执行：
/// 1. `probe()` — 重新解析 DTB，发现设备节点
/// 2. MMIO identity-map — 将每个设备的 MMIO 区域映射到内核地址空间
/// 3. 创建实例 + hub::register
/// 4. PLIC: Driver::init() + register_external（UART 中断路由的前置依赖）
/// 5. UART: Driver::init()（依赖 PLIC 已在 hub 中）
/// 6. CLINT: Driver::init() + register_internal
///
/// 全局中断（sie/sstatus）**不在**此函数中使能——由调用方在日志就绪后使能。
pub(crate) fn init() -> Result<(), DriverError> {
    // ── Step 0: DTB 设备发现 ───────────────────────────────
    mprintln!("[driver] probe start");
    probe();
    mprintln!("[driver] probe done");

    let cfg = crate::platform::get();

    // ── Step 1: Identity-map MMIO 设备到内核地址空间 ──────
    mprintln!("[driver] map_devices start");
    map_devices()?;
    mprintln!("[driver] map_devices done");

    // ── Step 2: 创建实例 + 注册到 hub ──────────────────────
    mprintln!("[driver] register devices start");
    tree::for_each(|dev| match dev.compatible {
        "riscv,plic0" | "sifive,plic-1.0.0" => {
            mprintln!("[driver]   register plic-0 base={:#x}", dev.base);
            plic::register("plic-0", dev.base.as_usize(), 1);
        }
        "ns16550a" => {
            mprintln!("[driver]   register uart-0 base={:#x}", dev.base);
            uart::register("uart-0", dev.base.as_usize(), dev.interrupt.unwrap_or(10));
        }
        "riscv,clint0" | "sifive,clint0" | "riscv,aclint-mtimer" => {
            mprintln!("[driver]   register clint-0 base={:#x}", dev.base);
            clint::register("clint-0", dev.base.as_usize(), cfg.timebase_frequency);
        }
        _ => {}
    });
    mprintln!("[driver] register devices done");

    // ── Step 3: PLIC 初始化 + 注册外部中断 ─────────────────
    mprintln!("[driver] PLIC init start");
    let plic = hub::get::<plic::Plic>("plic-0").ok_or(DriverError::NotFound("plic-0"))?;
    plic.init()?;
    mprintln!("[driver] PLIC register_external");
    hal::interrupt::register_external(plic);
    mprintln!("[driver] PLIC init done");

    // ── Step 4: UART 初始化（依赖 PLIC 已在 hub 中）────────
    mprintln!("[driver] UART init start");
    let uart_dev = hub::get::<uart::Uart>("uart-0").ok_or(DriverError::NotFound("uart-0"))?;
    uart_dev.init()?;
    mprintln!("[driver] UART init done");

    // ── Step 5: CLINT 初始化 + 注册内部中断 ─────────────────
    mprintln!("[driver] CLINT init start");
    let clint_dev = hub::get::<clint::Clint>("clint-0").ok_or(DriverError::NotFound("clint-0"))?;
    clint_dev.init()?;
    hal::interrupt::register_internal(clint_dev);
    mprintln!("[driver] CLINT init done");

    Ok(())
}

/// 将 DTB 发现的每个 MMIO 设备 identity-map 到内核地址空间。
///
/// 遍历 `tree::for_each` 发现的设备节点，为每个设备的 `[base, base+size)` 区间
/// 建立恒等映射（RW, 无 X, Global）。任一设备映射失败则立即返回错误——
/// MMIO 映射失败意味着页表资源耗尽或地址冲突，继续引导不安全。
///
/// # Errors
///
/// 任一设备的 MMIO 映射失败时返回 [`DriverError::MapFailed`]。
fn map_devices() -> Result<(), DriverError> {
    let guard = space::kernel_space();
    let ks = guard
        .as_ref()
        .expect("kernel space not initialized before driver init");
    let alloc = page::allocator();

    let dev_flags =
        PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D | PteFlags::G;

    let mut map_error: Option<DriverError> = None;

    tree::for_each(|dev| {
        if map_error.is_some() {
            return;
        }
        match ks.map_region(
            VirtAddr::from_raw(dev.base.as_usize()),
            dev.base,
            dev.size,
            dev_flags,
            alloc,
        ) {
            Ok(()) => {
                mprintln!(
                    "[driver]   mapped {} base={:#x} size={:#x}",
                    dev.compatible,
                    dev.base,
                    dev.size
                );
            }
            Err(e) => {
                mprintln!(
                    "[driver] ERROR: failed to map {} base={:#x} size={:#x}: {:?}",
                    dev.compatible,
                    dev.base,
                    dev.size,
                    e
                );
                map_error = Some(DriverError::MapFailed(dev.compatible));
            }
        }
    });

    if let Some(err) = map_error {
        return Err(err);
    }

    // 刷新 TLB 确保所有 MMIO 映射对后续访问可见
    unsafe {
        crate::memory::flush_tlb();
    }
    mprintln!("[driver] TLB flushed after map_devices");
    Ok(())
}
