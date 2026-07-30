// CLINT (Core Local Interruptor) — 定时器 + 软件中断
//
// MMIO 基址和定时器频率从 platform config 获取。
//   +0x0000  → MSIP (hart 0, 软件中断挂起位)
//   +0x4000  → MTIMECMP (hart 0, 定时器比较值)
//   +0xBFF8  → MTIME (64-bit 单调递增计数器, 只读)
//
// CLINT 产生两类中断，在 trap.rs 中以独立路径分发：
//   1 → SSI  → csrc sip（架构行为，不依赖 CLINT）
//   5 → STI  → Timer::handle_interrupt

use crate::hal::csr::sie::{self, Sie};
use crate::hal::{Driver, DriverError, Timer};
use crate::lock::OnceLock;
use crate::sbi;

/// CLINT 控制器——提供定时器和软件中断两组能力
#[derive(Debug)]
pub struct Clint {
    base: usize,
    timebase_freq: u64,
}

// 单核 M-mode 下，MMIO 指针跨上下文（主循环 + 中断）安全
unsafe impl Sync for Clint {}

impl Clint {
    pub const fn new(base: usize, timebase_freq: u64) -> Self {
        Clint {
            base,
            timebase_freq,
        }
    }

    // ── 定时器中断 ──────────────────────────────────────

    /// 使能监管者定时器中断 (sie.STIE)
    pub fn enable_timer_interrupt(&self) {
        unsafe { sie::set(Sie::STIE) };
    }

    /// 完整初始化定时器中断：使能 + 首次装载
    pub fn init_timer(&self) {
        self.enable_timer_interrupt();
        self.next(self.read() + self.frequency());
    }

    // ── 软件中断 ────────────────────────────────────────

    /// 触发软件中断（写 MSIP=1）
    pub fn trigger_soft_interrupt(&self) {
        let msip = self.base as *mut u32;
        unsafe {
            msip.write_volatile(1);
        }
    }

    /// 使能监管者软件中断 (sie.SSIE)
    pub fn enable_soft_interrupt(&self) {
        unsafe { sie::set(Sie::SSIE) };
    }
}

impl Timer for Clint {
    fn frequency(&self) -> u64 {
        self.timebase_freq
    }

    fn read(&self) -> u64 {
        // S-mode 下 CLINT MMIO (0x02000000) 被 OpenSBI PMP 阻止。
        // 使用 `time` CSR（Sstc 扩展）直接读取时间，与 mtime 等价。
        let t: u64;
        unsafe { core::arch::asm!("csrr {}, time", out(reg) t) };
        t
    }

    fn next(&self, abs: u64) {
        // S-mode 下无法写 mtimecmp (M-only)，通过 SBI ecall 委托 OpenSBI
        sbi::set_timer(abs);
    }

    fn handle_interrupt(&self) {
        debug!("timer tick");
        self.next(self.read().wrapping_add(self.frequency()));
    }
}

impl Driver for Clint {
    fn name(&self) -> &'static str {
        "riscv,clint0"
    }

    fn init(&self) -> Result<(), DriverError> {
        self.enable_timer_interrupt();
        self.next(self.read() + self.frequency());
        Ok(())
    }
}

static CLINT_INSTANCE: OnceLock<Clint> = OnceLock::new();

/// 初始化 CLINT 实例（引导早期调用一次）
pub(crate) fn init(base: usize, timebase_freq: u64) {
    CLINT_INSTANCE
        .set(Clint::new(base, timebase_freq))
        .expect("CLINT already initialized");
}

/// 获取 CLINT 实例引用
#[allow(non_snake_case)]
pub fn CLINT() -> &'static Clint {
    CLINT_INSTANCE.get().expect("CLINT not initialized")
}
