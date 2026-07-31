// 平台启动序列 — 在 main() 中调用一次
//
// 驱动初始化全部收敛到 `driver::init()`：
//   创建实例 → hub 注册 → Driver::init() → 中断控制器注册
//
// 全局中断使能延迟到日志就绪后，避免 CLINT timer tick 在 print 可用前触发。

use crate::{
    driver, filesystem,
    hal::{
        csr::{
            sie::{self, Sie},
            sstatus::{self, Sstatus},
        },
        InternalInterrupt,
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

    // ── Phase 1: 内存 & 驱动 & 陷阱基础设施 ─────────────────
    allocator::init();
    space::init().map_err(InitError::Memory)?;
    driver::init().map_err(InitError::Driver)?;
    trap::init();

    // ── Phase 2: VFS + 控制台 + 日志 ──────────────────────
    let root = filesystem::dev::create_devfs();
    filesystem::filetable::set_root(root);

    print::init();

    // 日志时间戳源：CLINT 已在 driver::init() 中初始化完毕
    let cfg = platform::get();
    log::init_timestamp(
        || {
            driver::hub::get::<driver::clint::Clint>("clint-0")
                .map(|c| c.read())
                .unwrap_or(0)
        },
        cfg.timebase_frequency,
    );
    log::set_max_level(log::LogLevel::Trace);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_probe_error();

    // ── Phase 3: 全局中断使能 ─────────────────────────────
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    sie::set(Sie::SEIE);
    sstatus::set(Sstatus::SIE);

    Ok(())
}
