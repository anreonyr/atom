// 控制台链路 — 输出（设备选择 → 带锁通道）与输入（设备选择 → 阻塞通道）
//
//    sink.rs    输出设备选择层：动态注册表 + preferred（Linux console_list 对应物）
//    source.rs  输入设备选择层：动态注册表 + preferred（stdin；无兜底 → Option）
//    print.rs   带锁输出通道：write/write_to + 宏 print!/println!/tprint!/tprintln!
//    input.rs   阻塞输入通道：read/read_byte/read_to（Linux tty_read 对应物）
//    uart.rs    UART 服务集成层：三视图（File/Write/Input）+ 三联动（sink/source/trap）
//
// 依赖方向：sink → fmt::Write + sbi；print → sink；source → lock + alloc；
// input → source + schedule + file；uart → file + hal + source + sink + trap
// 整体对外经 crate root 的 pub use 保持 crate::sink / crate::print / crate::uart
// 及新增 crate::source / crate::input 路径兼容。

pub mod sink;
pub mod source;

#[macro_use]
pub mod print;

pub mod input;
pub mod uart;
