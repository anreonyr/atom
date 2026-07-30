pub mod clint;
pub mod device;
pub mod plic;
pub mod uart;

// 只 re-export 访问器函数，具体类型名不对外暴露
pub use clint::CLINT;
pub use plic::PLIC;
pub use uart::UART;
