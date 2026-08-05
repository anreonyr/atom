// 控制台输出链路 — 设备选择 → 带锁通道 → UART 服务集成
//
//    sink.rs   打印设备选择层：动态注册表 + preferred（Linux console_list 对应物）
//    print.rs  带锁输出通道：write/write_to + 宏 print!/println!/tprint!/tprintln!
//    uart.rs   UART 服务集成层：注册表 + File/Write/InterruptHandler 适配
//
// 依赖方向：sink → fmt::Write + sbi；print → sink；uart → file + hal + sink
// 整体对外经 crate root 的 pub use 保持 crate::sink / crate::print / crate::uart 路径兼容。

pub mod sink;

#[macro_use]
pub mod print;

pub mod uart;
