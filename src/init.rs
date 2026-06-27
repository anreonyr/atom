// 平台启动序列 — 在 main() 中调用一次
//
// 初始化分为三个严格有序的阶段：
//   1. 内存 & 陷阱基础设施 — allocator、中断处理器注册表、陷阱向量
//   2. 控制台 — UART 硬件、设备注册、print 子系统（此后 println! 可用）
//   3. 中断子系统 — PLIC、各设备中断路由、定时器、全局中断使能
//
// 定时器 (MTI) 和软件中断 (MSI) 由 CLINT 统一管理，但在 trap.rs 中
// 以独立路径分发（mcause=7 / mcause=3），不再通过 IrqHandler trait 注册。

use crate::drivers::{device, CLINT, PLIC, UART};
use crate::hal::csr::mie::{self, Mie};
use crate::hal::csr::mstatus::{self, Mstatus};
use crate::hal::{InterruptController, Timer};

/// 运行完整的平台初始化序列
pub fn run() {
    unsafe {
        // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
        crate::allocator::init();   // 堆 + 物理帧
        crate::mmu::init();         // Sv39 分页（必须在 allocator 之后）
        crate::trap::init();        // 陷阱向量安装

        // ── Phase 2: 控制台（必须在任何 println! / 日志输出之前完成） ──
        UART.init_hw();
        device::register::<dyn core::fmt::Write>(&UART);
        crate::print::init();

        // 日志时间戳源：注册 CLINT mtime 读取函数
        crate::log::init_timestamp(|| CLINT.read_mtime());
        crate::log::set_max_level(crate::log::LogLevel::Trace);

        // ── Phase 3: 中断子系统 ──────────────────────────────
        PLIC.init();
        UART.init_irq();
        CLINT.init_timer();

        // 全局中断使能
        mie::set(Mie::MEIE); // MEIE: 机器外部中断使能
        mstatus::set(Mstatus::MIE); // MIE:  机器全局中断使能

        info!("interrupts enabled (MSI + MTI + MEI)");
    }
}
