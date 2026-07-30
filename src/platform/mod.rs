// 平台配置 — 从 DTB 探测或使用硬编码回退
//
// 引导流程：
//   1. `platform::init(dtb_ptr)` — 尝试解析 DTB，失败则缓存诊断并回退
//   2. `platform::config()` — 此后可安全调用，返回静态引用
//   3. `platform::report_diag()` — 日志就绪后输出诊断信息
//
// DTB 探测在 Phase 1（allocator / MMU）之前运行，解码期间仅使用栈变量。

pub mod dtb;

use crate::lock::BareLock;

/// RISC-V 页大小（所有 Sv 分页模式通用）。
pub const PAGE_SIZE: usize = 4096;

/// QEMU virt 平台默认配置（DTB 不可用时的回退值）。
pub mod qemu_virt {
    pub const DRAM_BASE: usize = 0x8000_0000;
    pub const DRAM_SIZE: usize = 8 * 1024 * 1024;  // 8 MiB
    pub const UART_BASE: usize = 0x1000_0000;
    pub const UART_INTERRUPT: u32   = 10;
    pub const CLINT_BASE: usize = 0x0200_0000;
    pub const PLIC_BASE: usize  = 0x0C00_0000;
    pub const PLIC_SIZE: usize  = 0x30_0000;        // 3 MiB, 覆盖 S-mode 上下文
    pub const TIMEBASE_FREQ: u64 = 10_000_000;       // 10 MHz
}

/// 平台硬件配置（只读，初始化后不可变）。
#[derive(Debug)]
pub struct PlatformConfig {
    /// DRAM 物理基址
    pub dram_base: usize,
    /// DRAM 总大小 (bytes)
    pub dram_size: usize,
    /// NS16550A UART MMIO 基址
    pub uart_base: usize,
    /// UART PLIC 中断号
    pub uart_interrupt: u32,
    /// CLINT MMIO 基址
    pub clint_base: usize,
    /// PLIC MMIO 基址
    pub plic_base: usize,
    /// PLIC MMIO 区域大小（须覆盖 S-mode context）
    pub plic_size: usize,
    /// 定时器频率 (Hz)，用于 CLINT ticks_per_sec
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
            dram_base: qemu_virt::DRAM_BASE,
            dram_size: qemu_virt::DRAM_SIZE,
            uart_base: qemu_virt::UART_BASE,
            uart_interrupt: qemu_virt::UART_INTERRUPT,
            clint_base: qemu_virt::CLINT_BASE,
            plic_base: qemu_virt::PLIC_BASE,
            plic_size: qemu_virt::PLIC_SIZE,
            timebase_freq: qemu_virt::TIMEBASE_FREQ,
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
/// 首选从 DTB 解析；若 `dtb_ptr` 为 0 或解析失败则回退到 QEMU virt 默认值，
/// 同时缓存诊断信息供后续日志输出。
///
/// # Safety
///
/// 必须在引导早期、单 hart 下调用恰好一次，在任何读取 `config()` 之前。
pub unsafe fn init(dtb_ptr: usize) {
    let mut cfg = if dtb_ptr != 0 {
        let probed = probe_dtb(dtb_ptr);
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

// ── DTB 探测 ────────────────────────────────────────────────

/// 从 DTB 解析平台配置。
///
/// # Safety
///
/// `dtb_ptr` 必须指向有效的 FDT 头部。
/// 从 DTB 解析平台配置。
///
/// # Safety
///
/// `dtb_ptr` 必须指向有效的 FDT 头部。
unsafe fn probe_dtb(dtb_ptr: usize) -> PlatformConfig {
    let dtb = match dtb::Dtb::new(dtb_ptr) {
        Ok(d) => d,
        Err(_) => return PlatformConfig::default_qemu_virt(),
    };

    let mut cfg = PlatformConfig::default_qemu_virt();

    // /memory — DRAM
    if let Some(mem) = dtb.find_type("memory") {
        if let Some((base, size)) = mem.property_reg(&dtb, 0) {
            cfg.dram_base = base as usize;
            cfg.dram_size = size as usize;
        }
    }

    // /cpus — timebase frequency
    if let Some(cpus) = dtb.find_path("/cpus") {
        if let Some(freq) = cpus.property_u32(&dtb, "timebase-frequency") {
            cfg.timebase_freq = freq as u64;
        }
    }

    // NS16550A UART
    if let Some(uart) = dtb.find_compatible("ns16550a") {
        if let Some((base, _)) = uart.property_reg(&dtb, 0) {
            cfg.uart_base = base as usize;
        }
        if let Some(irq) = uart.property_u32(&dtb, "interrupts") {
            cfg.uart_interrupt = irq;
        }
    }

    // RISC-V CLINT
    if let Some(clint) = dtb.find_compatible("riscv,clint0") {
        if let Some((base, _)) = clint.property_reg(&dtb, 0) {
            cfg.clint_base = base as usize;
        }
    }

    // RISC-V PLIC
    if let Some(plic) = dtb.find_compatible("riscv,plic0") {
        if let Some((base, size)) = plic.property_reg(&dtb, 0) {
            cfg.plic_base = base as usize;
            cfg.plic_size = size as usize;
        }
    }

    cfg
}
