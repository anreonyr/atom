// 平台启动序列 — 在 main() 中调用一次
//
// 所有 MMIO 驱动现在通过统一的 `Driver::init()` 初始化。
// 各驱动按依赖顺序排列：外部中断控制器 (PLIC) 必须先于 UART，
// 内部中断控制器 (CLINT) 在日志就绪后初始化。
//
// 初始化完成后注册到 hal::interrupt 中的 ExternalInterrupt 和
// InternalInterrupt 全局槽位，供 trap_handler 通过 trait 分发。

use crate::{
    drivers::{self, hub},
    filesystem,
    hal::{
        self,
        csr::{
            sie::{self, Sie},
            sstatus::{self, Sstatus},
        },
        Driver, InternalInterrupt,
    },
    log,
    memory::{self, allocator},
    panic, platform, print, trap,
};

/// 引导期错误 — `init::run()` 的返回值。
///
/// 遵循项目模式：模块级 `Error` + `type Result<T>` 别名，
/// 模块路径即命名空间（如 `init::Error`、`filesystem::Error`）。
#[derive(Debug)]
pub enum Error {
    /// 内存映射失败（页表分配、地址映射等）
    Memory(memory::table::MapError),
    /// hub 中未找到所需驱动
    DriverNotFound(&'static str),
    /// 驱动初始化失败
    DriverInit(crate::hal::driver::DriverError),
}

/// 模块级 `Result` 别名，遵循 filesystem::Result 模式。
pub type Result<T> = core::result::Result<T, Error>;

/// 运行完整的平台初始化序列。
///
/// # Errors
///
/// 引导失败时返回 [`Error`]；调用方（`main`）应终止执行。
pub unsafe fn run() -> Result<()> {
    // ── Phase 1: 内存 & 陷阱基础设施 ──────────────────────
    allocator::init();
    drivers::probe();
    memory::space::init().map_err(Error::Memory)?;
    trap::init();
    panic::set_verbosity(panic::PanicVerbosity::Full);

    // 从 DTB 发现缓冲区创建驱动静态实例
    drivers::init();

    // ── Phase 2: 驱动初始化 + 控制台 ─────────────────────
    let plic = hub::get::<drivers::plic::Plic>("plic-0").ok_or(Error::DriverNotFound("plic-0"))?;
    plic.init().map_err(Error::DriverInit)?;
    hal::interrupt::register_external(plic);

    let console_uart =
        hub::get::<drivers::uart::Uart>("uart-0").ok_or(Error::DriverNotFound("uart-0"))?;
    console_uart.init().map_err(Error::DriverInit)?;

    // 构建 VFS 命名空间（/dev/console, /dev/null, /dev/zero）
    let root = filesystem::dev::create_devfs();
    filesystem::filetable::set_root(root);

    print::init();

    // 日志时间戳源：注册 CLINT 时间读取 + 频率
    // CLINT 未就绪时返回 0 避免 panic（正常引导顺序下不会发生）。
    let cfg = platform::get();
    log::init_timestamp(
        || {
            drivers::hub::get::<drivers::clint::Clint>("clint-0")
                .map(|c| c.read())
                .unwrap_or(0)
        },
        cfg.timebase_frequency,
    );
    log::set_max_level(log::LogLevel::Trace);

    // 输出 DTB 解析诊断信息（如有）
    platform::report_probe_error();

    // ── Phase 3: 内部中断 + 全局中断使能 ─────────────────
    let clint =
        hub::get::<drivers::clint::Clint>("clint-0").ok_or(Error::DriverNotFound("clint-0"))?;
    clint.init().map_err(Error::DriverInit)?;
    hal::interrupt::register_internal(clint);

    // 全局中断使能
    // SAFETY: 单 hart，中断已禁用（刚完成初始化），写 CSR 是安全的。
    sie::set(Sie::SEIE);
    sstatus::set(Sstatus::SIE);

    Ok(())
}
