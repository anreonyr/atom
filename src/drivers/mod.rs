pub mod clint;
pub mod hub;
pub mod plic;
pub mod tree;
pub mod uart;

// probe 是唯一直供外部调用的发现入口
pub use tree::{for_each, probe};

/// 遍历 DTB 发现缓冲区，为每个已知设备创建驱动实例并注册到全局注册中心。
pub(crate) fn discover() {
    let cfg = crate::platform::config();
    tree::for_each(|dev| match dev.compatible {
        "riscv,plic0" | "sifive,plic-1.0.0" => {
            let p = plic::init(dev.base, 1);
            hub::register::<plic::Plic>(p, "plic-0");
        }
        "ns16550a" => {
            let u = uart::init(dev.base, dev.interrupt.unwrap_or(10));
            hub::register::<uart::Uart>(u, "uart-0");
        }
        "riscv,clint0" | "sifive,clint0" | "riscv,aclint-mtimer" => {
            let c = clint::init(dev.base, cfg.timebase_frequence);
            hub::register::<clint::Clint>(c, "clint-0");
        }
        _ => {}
    });
}
