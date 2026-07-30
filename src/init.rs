// 平台启动序列 — 在 main() 中调用一次
//
// 所有 MMIO 驱动现在通过统一的 `Driver::init()` 初始化。
// 各驱动按依赖顺序排列：外部中断控制器 (PLIC) 必须先于 UART，
// 内部中断控制器 (CLINT) 在日志就绪后初始化。
//
// 初始化完成后注册到 hal::interrupt 中的 ExternalInterrupt 和
// InternalInterrupt 全局槽位，供 trap_handler 通过 trait 分发。

use crate::{
    allocator,
    drivers::{self, hub},
    hal::{
        self,
        csr::{
            sie::{self, Sie},
            sstatus::{self, Sstatus},
        },
        Driver, InternalInterrupt,
    },
    log, mmu, panic, platform, print, trap,
};

/// 运行完整的平台初始化序列
pub fn run() {
    // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
    // SAFETY: 引导早期单 hart 调用一次，无并发。
    unsafe {
        allocator::init();
        drivers::probe();
        mmu::init();
        trap::init();
    }
    panic::set_verbosity(panic::PanicVerbosity::Full);

    // 从 DTB 发现缓冲区创建驱动静态实例
    drivers::discover();

    // ── Phase 2: 驱动初始化 + 控制台 ─────────────────────
    let plic = hub::get::<drivers::plic::Plic>("plic-0").expect("PLIC not found");
    plic.init().expect("PLIC init failed");
    hal::interrupt::register_external(plic);

    let console_uart = hub::get::<drivers::uart::Uart>("uart-0").expect("UART not found");
    console_uart.init().expect("UART init failed");
    print::init();

    // 日志时间戳源：注册 CLINT 时间读取 + 频率
    let cfg = platform::config();
    log::init_timestamp(
        || {
            drivers::hub::get::<drivers::clint::Clint>("clint-0")
                .expect("CLINT init before clint-0 registered")
                .read()
        },
        cfg.timebase_frequence,
    );
    log::set_max_level(log::LogLevel::Trace);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_diag();

    // ── Phase 3: 内部中断 + 全局中断使能 ─────────────────
    let clint = hub::get::<drivers::clint::Clint>("clint-0").expect("CLINT not found");
    clint.init().expect("CLINT init failed");
    hal::interrupt::register_internal(clint);

    // 全局中断使能
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    unsafe {
        sie::set(Sie::SEIE);
        sstatus::set(Sstatus::SIE);
    }
}
