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
/// 调用各驱动的 `probe()` 从 DTB 设备发现缓冲区匹配并创建静态实例。
/// 此时不接触硬件——`Driver::init()` 在 `probe_all` 返回后按序调用。
pub(crate) fn probe_all() {
    plic::probe();
    uart::probe();
    clint::probe();
}
