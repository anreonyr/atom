// 硬件抽象层 — 全部硬件能力契约的集合
//
// hal/ 定义内核与硬件之间的能力契约（纯 trait + 注册表，零依赖服务层）：
//   interrupt.rs 内部/外部中断控制器能力（InternalInterrupt/ExternalInterrupt）
//                + 设备中断处理器能力（InterruptHandler）+ 注册表
//   uart.rs      UART 硬件能力契约（Uart；服务集成在 src/uart.rs）
//   rtc.rs       墙上时钟硬件能力契约（Realtime）+ 注册表
//   csr.rs       S-mode CSR 包装（sstatus/sie/stvec/scause/...）
//   cpu.rs       HartId（单 hart 桩）
// 服务层（clock/schedule/log 等）消费 hal 能力，不反向依赖。

pub mod cpu;
pub mod csr;
pub mod interrupt;
pub mod rtc;
pub mod uart;

pub use interrupt::{ExternalInterrupt, InternalInterrupt, InterruptHandler};
