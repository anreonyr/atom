// 平台启动序列 — 在 main() 中调用一次
//
// 初始化分为三个严格有序的阶段：
//   1. 内存 & 陷阱基础设施 — allocator、中断处理器注册表、陷阱向量
//   2. 控制台 — UART 硬件、设备注册、print 子系统（此后 println! 可用）
//   3. 中断子系统 — PLIC、各设备中断路由、定时器、全局中断使能

use core::arch::asm;

use crate::drivers::{device, CLINT, PLIC, UART};
use crate::hal::InterruptController;

/// 运行完整的平台初始化序列
pub fn run() {
    // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
    crate::allocator::init();
    crate::trap::init_handlers();
    csr_write!(mtvec, crate::trap::trap_vector as *const () as usize);

    // ── Phase 2: 控制台（必须在任何 println! 之前完成） ──
    UART.init_hw();
    device::register::<dyn core::fmt::Write>(&UART);
    crate::print::init();

    println!("atom kernel booted");

    // ── Phase 3: 中断子系统 ──────────────────────────────
    PLIC.init();
    UART.init_irq();
    CLINT.init_timer();

    // 全局中断使能
    csr_set!(mie, 1 << 11);   // MEIE: 机器外部中断使能
    csr_set!(mstatus, 1 << 3); // MIE:  机器全局中断使能

    println!("[info] interrupts enabled (MTI + MEI)");
}
