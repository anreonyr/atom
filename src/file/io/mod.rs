// io — 终端标准流域（std::io 对应物）
//
//   device.rs  终端设备选择层：统一设备表（writer+buffer 双视图）+ preferred
//              + sbi 常驻 + InputBuffer（原 console/device.rs）
//   print.rs   输出通道：OUT 锁 + print!/println!/mprint! 宏家族（原 console/print.rs）
//   uart.rs    UART 服务集成层：注册表 + Write/Input/InterruptHandler 适配
//              （原 console/uart.rs）
//   stdio.rs   stdin/stdout 句柄：Stdin/Stdout（File 视图 + 阻塞句柄方法）
//
// 依赖方向：device → fmt::Write + sbi；print → device；stdio → device + print +
// file::vfs::filetable + schedule；uart → hal + device + trap + file::ops。
// 本域不反向依赖提供方（driver/log 只注册、只消费契约）。

pub mod console;
pub mod device;
#[macro_use]
pub mod print;
pub mod stdio;
pub mod uart;

// std::io 形态：crate::io::stdin() / crate::io::stdout()
pub use stdio::{stdin, stdout, Stdin, Stdout};
