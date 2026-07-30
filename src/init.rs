// 平台启动序列 — 在 main() 中调用一次
//
// 所有 MMIO 驱动现在通过统一的 `Driver::init()` 初始化。
// 各驱动按依赖顺序排列：外部中断控制器 (PLIC) 必须先于 UART，
// 内部中断控制器 (CLINT) 在日志就绪后初始化。
//
// 初始化完成后注册到 hal::interrupt 中的 ExternalInterrupt 和
// InternalInterrupt 全局槽位，供 trap_handler 通过 trait 分发。

use crate::drivers::{device, CLINT, PLIC, UART};
use crate::hal::csr::sie::{self, Sie};
use crate::hal::csr::sstatus::{self, Sstatus};
use crate::hal::{Driver, InternalInterrupt};
use crate::platform;

/// 运行完整的平台初始化序列
pub fn run() {
    // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
    // SAFETY: 引导早期单 hart 调用一次，无并发。
    unsafe {
        crate::allocator::init();
        platform::dev::discover();
        crate::mmu::init();
        crate::trap::init();
    }
    crate::panic::set_verbosity(crate::panic::PanicVerbosity::Full);

    // 从 DTB 发现缓冲区创建驱动静态实例
    crate::drivers::probe_all();

    // ── Phase 2: 驱动初始化 + 控制台 ─────────────────────
    PLIC().init().expect("PLIC init failed");
    crate::hal::interrupt::register_external(PLIC());

    UART().init().expect("UART init failed");
    device::register::<dyn core::fmt::Write>(UART());
    crate::print::init();

    // 日志时间戳源：注册 CLINT 时间读取 + 频率
    let cfg = platform::config();
    crate::log::init_timestamp(|| CLINT().read(), cfg.timebase_freq);
    crate::log::set_max_level(crate::log::LogLevel::Trace);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_diag();

    // ── Phase 3: 内部中断 + 全局中断使能 ─────────────────
    CLINT().init().expect("CLINT init failed");
    crate::hal::interrupt::register_internal(CLINT());

    // 全局中断使能
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    unsafe {
        sie::set(Sie::SEIE);
        sstatus::set(Sstatus::SIE);
    }
}
