// print!/println! — 格式化输出通道（输出路由 + 格式化宏）
//
// 分层：hal::ByteChannel ← file/io/console（终端核心）← print（输出路由）← log / 各模块
//
// print.rs 回答一个问题："格式化结果打向哪"：
//   - 解析 /dev/console 符号链接（VFS resolve，零锁）→ 目标 &dyn File 写入
//     （持 OUT 锁串行化；CRLF 由终端核心 Console::File::write 转换）
//   - 链接不存在（boot 早期 devfs 根未建 / 无终端注册）→ sbi 无锁直写（M-mode）
//
// "根未建即早期"：终端注册全部需要 allocator（register 的 Box::leak/format!），
// allocator 未就绪时 devfs 必然未建，resolve 返回 None，自然回落 sbi——无需
// 显式阶段状态机（Early/Ready 的职责由"链接不存在"这一数据结构本身表达）。
//
// 无锁输出（panic / lockdep）不经本模块——调用方直接 `sbi::mprintln!`
// （sbi 模块，M-mode 控制台直写，见 src/sbi/mod.rs）。

use core::fmt;
use core::fmt::Write;

use crate::file::ops::File;
use crate::file::vfs::filetable;
use crate::lock::SpinLock;

/// 输出串行化锁 — 关中断，保证唯一写者。与 VFS 路径解析不同时持有
/// （`filetable::resolve` 零锁，OnceLock + lookup 线性扫），锁序无嵌套。
static OUT: SpinLock<()> = SpinLock::new(());

/// fmt::Write 桥 — 把格式化渲染的每个字符串片转发到 `&dyn File`。
///
/// 本地新类型（孤儿规则）：`File` 是本地 trait，但其 `write` 不满足
/// `fmt::Write` 的 `&mut self` 签名形态，桥在此补一层。CRLF 是逐字符无跨片
/// 状态变换，多次 `write_str` 等价整段一次。
struct FileFmt<'a>(&'a dyn File);

impl fmt::Write for FileFmt<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0
            .write(0, s.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

/// 格式化输出 — 决策目标：解析 /dev/console 链接 → 终端 File（带锁）；
/// 链接不存在 → sbi 无锁直写。
pub fn write(args: fmt::Arguments) {
    match filetable::resolve("/dev/console") {
        Some(file) => {
            let _guard = OUT.lock();
            let _ = FileFmt(file).write_fmt(args);
        }
        None => crate::sbi::write_fmt(args),
    }
}

/// 原始字节输出 — 同上（字节语义，非 UTF-8 原样直写，`\n` → `\r\n` 由目标转换）。
pub fn write_bytes(bytes: &[u8]) {
    match filetable::resolve("/dev/console") {
        Some(file) => {
            let _guard = OUT.lock();
            let _ = file.write(0, bytes);
        }
        None => crate::sbi::write_bytes(bytes),
    }
}

/// 输出到指定路径（VFS 路径，如 "/dev/uart1"）— 目标不存在静默丢弃。
///
/// 预留：tprint!/tprintln! 的核心，当前无调用方。路径是 VFS 名而非设备表名
/// （设备表已随终端核心提炼移除）。
#[allow(dead_code)]
pub fn twrite(name: &'static str, args: fmt::Arguments) {
    let _guard = OUT.lock();
    let Some(file) = filetable::resolve(name) else {
        return;
    };
    let _ = FileFmt(file).write_fmt(args);
}

/// 格式化输出，无换行 — 目标为 preferred 终端（/dev/console 链接）。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::io::print::write(format_args!($($arg)*));
    }};
}

/// 格式化输出，自动附加换行 — 目标为 preferred 终端。
#[macro_export]
macro_rules! println {
    () => { $crate::io::print::write(format_args!("\n")) };
    ($($arg:tt)*) => {{
        $crate::io::print::write(format_args!("{}\n", format_args!($($arg)*)));
    }};
}

/// 格式化输出到指定路径，无换行 — 目标不存在静默。
#[macro_export]
macro_rules! tprint {
    ($dev:expr, $($arg:tt)*) => {{
        $crate::io::print::twrite($dev, format_args!($($arg)*));
    }};
}

/// 格式化输出到指定路径，自动附加换行 — 目标不存在静默。
#[macro_export]
macro_rules! tprintln {
    ($dev:expr) => { $crate::io::print::twrite($dev, format_args!("\n")) };
    ($dev:expr, $($arg:tt)*) => {{
        $crate::io::print::twrite($dev, format_args!("{}\n", format_args!($($arg)*)));
    }};
}
