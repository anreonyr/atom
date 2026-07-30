pub mod clint;
pub mod device;
pub mod tree;
pub mod plic;
pub mod uart;

// 只 re-export 访问器函数，具体类型名不对外暴露
pub use clint::CLINT;
pub use tree::{discover, for_each};
pub use plic::PLIC;
pub use uart::UART;

/// 遍历 DTB 发现缓冲区，为每个已知设备创建驱动实例。
pub(crate) fn probe() {
    let cfg = crate::platform::config();
    for_each(|dev| {
        match dev.compatible {
            "riscv,plic0" | "sifive,plic-1.0.0" =>
                plic::init(dev.base, 1),
            "ns16550a" =>
                uart::init(dev.base, dev.interrupt.unwrap_or(10)),
            "riscv,clint0" | "sifive,clint0" | "riscv,aclint-mtimer" =>
                clint::init(dev.base, cfg.timebase_freq),
            _ => {}
        }
    });
}
