// 平台启动序列 — 在 main() 中调用一次
//
// 所有 MMIO 驱动现在通过统一的 `Driver::init()` 初始化。
// 各驱动按依赖顺序排列：PLIC 必须先于 UART（UART 的 init 需要配置 PLIC 路由），
// CLINT 在日志就绪后初始化。初始化完成后注册到 hal::{timer, interrupt}。
//
// 定时器 (STI) 和软件中断 (SSI) 通过 hal 层 trait 分发，
// trap_handler 不再直接引用具体驱动类型。

use crate::drivers::{device, CLINT, PLIC, UART};
use crate::hal::csr::sie::{self, Sie};
use crate::hal::csr::sstatus::{self, Sstatus};
use crate::hal::{Driver, Timer};
use crate::platform;

/// 运行完整的平台初始化序列
pub fn run() {
    // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
    // SAFETY: 引导早期单 hart 调用一次，无并发。
    unsafe {
        crate::allocator::init();
        crate::mmu::init();
        crate::trap::init();
    }
    crate::panic::set_verbosity(crate::panic::PanicVerbosity::Full);

    // 从 platform config 初始化驱动静态实例
    let cfg = platform::config();
    crate::drivers::uart::init(cfg.uart_base);
    crate::drivers::clint::init(cfg.clint_base, cfg.timebase_freq);
    crate::drivers::plic::init(cfg.plic_base, 1);

    // ── Phase 2: 驱动初始化 + 控制台 ─────────────────────
    PLIC().init().expect("PLIC init failed");
    // PLIC 必须在 UART 之前完成初始化（UART 的 init 配置 PLIC 路由）
    crate::hal::interrupt::register(PLIC());

    UART().init().expect("UART init failed");
    device::register::<dyn core::fmt::Write>(UART());
    crate::print::init();

    // 日志时间戳源：注册定时器读取函数 + 频率
    crate::log::init_timestamp(|| CLINT().read(), cfg.timebase_freq);
    crate::log::set_max_level(crate::log::LogLevel::Trace);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_diag();

    // ── Phase 3: 定时器 + 全局中断使能 ───────────────────
    CLINT().init().expect("CLINT init failed");
    crate::hal::timer::register(CLINT());

    // 全局中断使能
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    unsafe {
        sie::set(Sie::SEIE);
        sstatus::set(Sstatus::SIE);
    }
}
