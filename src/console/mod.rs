// 控制台链路 — 设备选择（统一表）→ 带锁输出通道 / 阻塞输入通道
//
//    device.rs  console 设备选择层：统一注册表（条目读写双视图）+ 单一
//              preferred + sbi 常驻（Linux tty 设备读写一体的对应物）
//    print.rs  带锁输出通道：write/write_to + 宏 print!/println!/tprint!/tprintln!
//    read.rs   阻塞输入通道：read/read_byte/read_to + Console（stdin/stdout 统一
//              File 视图，read 走 preferred_buffer / write 走 preferred_writer）
//    uart.rs   UART 服务集成层：Write/Input 两视图 + 联动（device/trap）
//
// 依赖方向：device → fmt::Write + sbi；print → device；read → device +
// schedule + file；uart → file + hal + device + trap
// 整体对外经 crate root 的 pub use 保持 crate::device / crate::print /
// crate::read / crate::uart 路径兼容。

pub mod device;

#[macro_use]
pub mod print;

pub mod read;
pub mod uart;
