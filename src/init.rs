// 平台启动序列 — 在 main() 中调用一次
//
// 驱动初始化全部收敛到 `driver::init()`：
//   创建实例 → hub 注册 → Driver::init() → 中断控制器注册
//
// 全局中断使能延迟到日志就绪后，避免 CLINT timer tick 在 print 可用前触发。

use crate::{
    driver, filesystem,
    memory::{allocator, space, table},
    trap,
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
    unsafe {
        // ── Phase 1: 内存 & 陷阱 & 驱动基础设施 ─────────────────
        allocator::init();
        info!("allocator ready");

        space::init().map_err(InitError::Memory)?;
        // SAFETY: space::init 已写入 KERNEL_SPACE，根页表必然存在。
        let root_ppn = space::kernel_space()
            .as_ref()
            .expect("kernel address space not initialized")
            .root();
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
            info!("bound {compatible} → {driver}");
        }

        // ── Phase 2: VFS + 控制台 + 日志 ──────────────────────
        let root = filesystem::dev::create_devfs();
        filesystem::filetable::set_root(root);
        let uarts = crate::device::count();
        info!("devfs ready — {uarts} console(s) + log/null/zero under /dev");

        // 预置 stdio：fd 0 = /dev/stdin（console 输入）、fd 1 = /dev/console0
        // （首个 UART，即 preferred 输出）。filetable::open 顺序分配 fd 0、1，
        // 此后 U 任务 read/write 经 VFS 全局表解析 fd（第 2 波进程化再移入
        // per-task 表）。失败仅告警：无 UART 时 U 任务 read/write 得 -EBADF。
        if let Err(e) = filesystem::filetable::open("/dev/stdin", crate::file::OpenFlags::READ) {
            warn!("stdio: preset fd 0 (/dev/stdin) failed: {e:?}");
        }
        if let Err(e) = filesystem::filetable::open("/dev/stdout", crate::file::OpenFlags::WRITE) {
            warn!("stdio: preset fd 1 (/dev/stdout) failed: {e:?}");
        }

        // 输出目标已由 sink 管理：Phase 1 中首个 UART probe 注册时自动成为
        // preferred（此前注册表为空时输出回落 SBI），此处无需显式切换。
        info!("console ready — {uarts} UART(s) registered");

        Ok(())
    }
}
