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

impl core::fmt::Display for InitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // 读取 payload（MapError/DriverError）：输出路径按字段读取，消除 dead_code
            InitError::Memory(e) => write!(f, "memory init failed: {e:?}"),
            InitError::Driver(e) => write!(f, "driver init failed: {e:?}"),
        }
    }
}

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
    log::set_clock_source(&log::CSR_CLOCK, platform::get().timebase_frequency);
    log::set_max_level(log::LogLevel::Debug);
    // 模块级过滤：clint 的 timer tick (debug) 每周期刷屏，抑制到 Info 以下；
    // 其余模块回落全局 Trace（最长前缀匹配，无命中时回落全局级别）
    log::set_module_rules(&[log::ModuleRule {
        prefix: "driver::controller::clint",
        level: log::LogLevel::Info,
    }]);
    info!(
        "log ready — level {:?} (clint timer capped at Info)",
        log::max_level()
    );

    // ── Phase 1: 内存 & 陷阱 & 驱动基础设施 ─────────────────
    allocator::init();
    info!("allocator ready (bump → hybrid)");

    space::init().map_err(InitError::Memory)?;
    // SAFETY: space::init 已写入 KERNEL_SPACE，根页表必然存在。
    let root_ppn = space::kernel_space()
        .as_ref()
        .expect("kernel address space not initialized")
        .root_page();
    info!(
        "address space ready (Sv39 root table {:#x})",
        root_ppn << crate::memory::PAGE_SHIFT
    );

    // trap 必须先于 driver：设备探测会访问 DTB/MMIO，若期间异常而 stvec 未就位，
    // trap 会重入 _start 重新执行 early（参数变垃圾值），产生误导性崩溃。
    trap::init();
    info!(
        "trap vector installed at {:#x}",
        trap::trap_vector as *const () as usize
    );

    driver::init().map_err(InitError::Driver)?;
    let (total, bound, unsupported) = driver::hub::get().device_summary();
    info!("drivers ready — {total} devices ({bound} bound, {unsupported} unsupported)");
    for (compatible, driver) in driver::hub::get().bound_devices() {
        info!("  bound {compatible} → {driver}");
    }

    // ── Phase 2: VFS + 控制台 + 日志 ──────────────────────
    let root = filesystem::dev::create_devfs();
    filesystem::filetable::set_root(root);
    let uarts = crate::uart::all().len();
    info!("devfs ready — {uarts} console(s) + log/null/zero under /dev");

    print::init();
    info!("console ready — {uarts} UART(s) registered");

    // ── Phase 3: 全局中断使能 ─────────────────────────────
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    // 定时器中断：mtimecmp 已在 CLINT probe 装载到未来，此处使能不会立即触发。
    sie::set(Sie::SEIE);
    sie::set(Sie::STIE);
    sie::set(Sie::SSIE);
    sstatus::set(Sstatus::SIE);
    // SAFETY: 单 hart，刚完成使能，读 CSR 无副作用。
    let sie_val = unsafe { sie::read() };
    let sstatus_val = unsafe { sstatus::read() };
    info!(
        "interrupts enabled — sie={:#x} (SEIE|STIE|SSIE), sstatus.SIE={}",
        sie_val.bits(),
        sstatus_val.contains(Sstatus::SIE) as u8,
    );

    Ok(())
}
