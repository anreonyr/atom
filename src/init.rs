// 平台启动序列 — 在 main() 中调用一次
//
// 初始化分为三个严格有序的阶段：
//   1. 内存 & 陷阱基础设施 — allocator、中断处理器注册表、陷阱向量
//   2. 控制台 — UART 硬件、设备注册、print 子系统（此后 println! 可用）
//   3. 中断子系统 — PLIC、各设备中断路由、定时器、全局中断使能
//
// 定时器 (STI) 和软件中断 (SSI) 由 CLINT 统一管理，但在 trap.rs 中
// 以独立路径分发（scause=5 / scause=1），不再通过 IrqHandler trait 注册。
//
// 驱动静态实例在 Phase 2 开始时根据 platform config 初始化。

use crate::drivers::{device, CLINT, PLIC, UART};
use crate::hal::csr::sie::{self, Sie};
use crate::hal::csr::sstatus::{self, Sstatus};
use crate::hal::{InterruptController, Timer};
use crate::platform;

/// 运行完整的平台初始化序列
///
/// # Safety
///
/// 必须在主 hart 引导早期调用一次，在中断使能前完成所有写 side-effect。
#[allow(static_mut_refs)]
pub fn run() {
    unsafe {
        // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
        crate::allocator::init();
        crate::mmu::init();
        crate::trap::init();

        // 从 platform config 初始化驱动静态实例
        let cfg = platform::config();
        UART = crate::drivers::Uart::new(cfg.uart_base);
        CLINT = crate::drivers::Clint::new(cfg.clint_base, cfg.timebase_freq);
        PLIC = crate::drivers::Plic::new(cfg.plic_base, 1);

        UART.init_hw();
        device::register::<dyn core::fmt::Write>(&UART);
        crate::print::init();

        // 日志时间戳源：注册 CLINT mtime 读取函数 + 频率
        crate::log::init_timestamp(|| CLINT.read_mtime(), cfg.timebase_freq);
        crate::log::set_max_level(crate::log::LogLevel::Trace);

        // 输出 DTB 解析诊断信息（如有）
        platform::report_diag();

        // ── Phase 3: 中断子系统 ──────────────────────────────
        PLIC.init();
        UART.init_irq();
        CLINT.init_timer();

        // 全局中断使能
        sie::set(Sie::SEIE); // SEIE: 监管者外部中断使能
        sstatus::set(Sstatus::SIE); // SIE:  监管者全局中断使能

        info!("interrupts enabled (SSI + STI + SEI)");
    }
}
