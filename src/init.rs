// 平台启动序列 — 在 main() 中调用一次
//
// 驱动初始化全部收敛到 `driver::init()`：
//   创建实例 → hub 注册 → Driver::init() → 中断控制器注册
//
// 全局中断使能延迟到日志就绪后，避免 CLINT timer tick 在 print 可用前触发。

use crate::{
    driver, filesystem,
    hal::csr::{
        sie::{self, Sie},
        sstatus::{self, Sstatus},
    },
    log,
    memory::{allocator, space, table},
    panic, platform, print, trap,
};

/// 引导期错误 — `init::run()` 的返回值。
#[derive(Debug)]
pub enum InitError {
    /// 内存映射失败（页表分配、地址映射等）
    Memory(table::MapError),
    /// 驱动子系统初始化失败
    Driver(driver::DriverError),
}

/// 模块级 `Result` 别名。
pub type Result<T> = core::result::Result<T, InitError>;

/// Run the full platform initialization sequence.
///
/// # Safety
///
/// Must be called exactly once, on the boot hart, with:
/// - interrupts disabled (`sstatus.SIE` clear)
/// - `satp` set to Bare (paging disabled)
/// - stack identity-mapped
/// - allocator not yet initialized
///
/// # Errors
///
/// Returns [`InitError`] on boot failure; the caller (`main`) should halt.
pub unsafe fn run() -> Result<()> {
    panic::set_verbosity(panic::PanicVerbosity::Full);

    // 日志时间戳源：直接读 time CSR（Sstc），init 序列最早即注册，
    // 让整个初始化过程（含驱动/console 就绪前）的日志都带真实时间戳
    log::init_timestamp(&log::CSR_CLOCK, platform::get().timebase_frequency);

    info!("init: phase 1 — allocator, address space, drivers, traps");
    // ── Phase 1: 内存 & 驱动 & 陷阱基础设施 ─────────────────
    allocator::init();
    space::init().map_err(InitError::Memory)?;
    driver::init().map_err(InitError::Driver)?;
    trap::init();

    info!("init: phase 2 — vfs, console, log");
    // ── Phase 2: VFS + 控制台 + 日志 ──────────────────────
    let root = filesystem::dev::create_devfs();
    filesystem::filetable::set_root(root);

    print::init();

    log::set_max_level(log::LogLevel::Trace);
    // 模块级过滤：clint 的 timer tick (debug) 每周期刷屏，抑制到 Info 以下；
    // 其余模块回落全局 Trace（最长前缀匹配，无命中时回落全局级别）
    log::set_module_rules(&[log::ModuleRule {
        prefix: "driver::controller::clint",
        level: log::LogLevel::Info,
    }]);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_probe_error();

    info!("init: phase 3 — enable global interrupts");
    // ── Phase 3: 全局中断使能 ─────────────────────────────
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    // 定时器中断：mtimecmp 已在 CLINT probe 装载到未来，此处使能不会立即触发。
    sie::set(Sie::SEIE);
    sie::set(Sie::STIE);
    sie::set(Sie::SSIE);
    sstatus::set(Sstatus::SIE);

    Ok(())
}
