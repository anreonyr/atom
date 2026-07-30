pub mod clint;
pub mod device;
pub mod plic;
pub mod uart;

// 只 re-export 访问器函数，具体类型名不对外暴露
pub use clint::CLINT;
pub use plic::PLIC;
pub use uart::UART;

/// 按引导依赖顺序发现并创建所有驱动实例。
///
/// 为每个驱动类型调用 `<T as Driver>::probe()` 从 DTB 匹配并创建静态实例。
/// 此时不接触硬件——`Driver::init()` 在 `probe_all` 返回后按序调用。
pub(crate) fn probe_all() {
    fn probe_one<T: crate::hal::Driver>() {
        if let Some(dev) = crate::platform::find_device(T::compatible()) {
            T::probe(dev);
        }
    }
    probe_one::<plic::Plic>();
    probe_one::<uart::Uart>();
    probe_one::<clint::Clint>();
}
