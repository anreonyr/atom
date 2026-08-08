// io — 终端标准流域（std::io 对应物）
//
//   console.rs 终端核心：设备无关的终端服务（Console/InputHandler/RawFile +
//              InputBuffer + register，`&dyn ByteChannel` 能力擦除，非泛型）
//   print.rs   输出路由 + 格式化宏：OUT 锁 + resolve(/dev/console) + print! 宏家族
//   stdio.rs   stdin/stdout 句柄：Stdin/Stdout（File 视图 + 阻塞句柄方法）
//
// 依赖方向：console → hal::byte_channel + trap + schedule + file::ops；
// print → file::vfs::filetable + sbi；stdio → file::vfs::filetable + print +
// schedule。本域不反向依赖提供方（driver/log 只注册、只消费契约）。

pub mod console;
#[macro_use]
pub mod print;
pub mod stdio;

// std::io 形态：crate::io::stdin() / crate::io::stdout()
pub use stdio::{stdin, stdout, Stdin, Stdout};
