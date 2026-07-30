// 平台模块 — DTB 解析、全局硬件配置
//
// 引导流程：
//   1. `platform::init(dtb_ptr)` — 解析 DTB，填充 PlatformConfig，保存 DTB 指针
//   2. `platform::config()` — 返回全局硬件配置（DRAM、timebase 等）
//   3. `platform::report_diag()` — 日志就绪后输出 DTB 诊断信息
//
// 设备发现由 `drivers::discovery` 模块完成（重解析 DTB + Vec）。

mod cell;
pub mod config;
mod dtb;
mod header;

/// RISC-V 页大小（所有 Sv 分页模式通用）。
pub const PAGE_SIZE: usize = 4096;

pub use config::{config, init, report_diag};
pub use dtb::Dtb;

/// DTB 不可用时的回退默认值。
pub mod qemu_virt {
    pub const DRAM_BASE: usize = 0x8000_0000;
    pub const DRAM_SIZE: usize = 8 * 1024 * 1024; // 8 MiB
    pub const UART_BASE: usize = 0x1000_0000;
    pub const UART_SIZE: usize = 0x1000;
    pub const UART_INTERRUPT: u32 = 10;
    pub const CLINT_BASE: usize = 0x0200_0000;
    pub const CLINT_SIZE: usize = 0x0001_0000;
    pub const PLIC_BASE: usize = 0x0C00_0000;
    pub const PLIC_SIZE: usize = 0x30_0000; // 3 MiB, 覆盖 S-mode 上下文
    pub const TIMEBASE_FREQ: u64 = 10_000_000; // 10 MHz
}
