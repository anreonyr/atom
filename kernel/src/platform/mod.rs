// 平台模块 — DTB 解析、全局硬件配置
//
// 引导流程：
//   1. `platform::init(dtb_ptr)` — 校验 DTB、解析全局参数，保存 DTB 句柄
//   2. `platform::get()` — 返回全局硬件配置（DRAM、timebase 等）
//   3. `platform::config::dtb()` — 供 allocator 就绪后的设备发现重解析
//
// DTB 缺失或无效时回退 qemu_virt 默认值，错误在 init 内即时输出。
// 设备发现由 `driver::device` 模块完成（复用 DTB 句柄 + Vec）。

mod cell;
pub mod config;
mod dtb;
mod header;

/// RISC-V 页大小（所有 Sv 分页模式通用）。
pub const PAGE_SIZE: usize = 4096;

pub use config::{Config, get, init};
pub use dtb::Dtb;

/// DTB 不可用时的回退默认值。
pub mod qemu_virt {
    pub const DRAM_BASE: usize = 0x8000_0000;
    pub const DRAM_SIZE: usize = 128 * 1024 * 1024; // 128 MiB (QEMU virt 默认)
    pub const UART_BASE: usize = 0x1000_0000;
    pub const UART_SIZE: usize = 0x1000;
    pub const UART_INTERRUPT: u32 = 10;
    pub const CLINT_BASE: usize = 0x0200_0000;
    pub const CLINT_SIZE: usize = 0x0001_0000;
    pub const PLIC_BASE: usize = 0x0C00_0000;
    pub const PLIC_SIZE: usize = 0x30_0000; // 3 MiB, 覆盖 S-mode 上下文
    pub const TIMEBASE_FREQUENCY: u64 = 10_000_000; // 10 MHz
}
