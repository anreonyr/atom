// print!/println! — 全局输出宏
//
// 从设备注册中心获取 `&dyn Write` 实例输出。
// OUTPUT 锁保证多核下输出不交错；WRITER 在 init() 写入一次，此后只读。
//
// 为了避免 invalid_reference_casting，在 init() 时将引用转成裸指针
// 存入 WRITER，此后 `outs()` 中只从裸指针拿 `&mut`，编译器不追踪源头。

use crate::lock::{OnceLock, SpinLock};

// 裸指针包装：init() 写入一次，之后只读，跨 hart 共享安全。
struct WriterPtr(*mut dyn core::fmt::Write);

// SAFETY: WRITER 经 OnceLock 写入一次后不再变动；输出串行化由 OUTPUT 保证。
unsafe impl Send for WriterPtr {}
unsafe impl Sync for WriterPtr {}

// 输出设备裸指针，写一次读多次（读路径无锁）
static WRITER: OnceLock<WriterPtr> = OnceLock::new();

// 输出串行化锁
static OUTPUT: SpinLock<()> = SpinLock::new(());

/// 初始化输出设备（在 device::register 之后调用）
pub fn init() {
    let uart = crate::drivers::hub::get::<crate::drivers::uart::Uart>("uart-0")
        .expect("UART-0 not registered");
    let w: &dyn core::fmt::Write = uart;
    // 一次性的 &T → *mut 转换
    let w = w as *const dyn core::fmt::Write as *mut dyn core::fmt::Write;
    WRITER.set(WriterPtr(w)).ok();
}

/// 在锁保护下执行输出闭包
pub fn outs<F>(f: F)
where
    F: FnOnce(&mut dyn core::fmt::Write),
{
    let _out_guard = OUTPUT.lock();
    if let Some(w) = WRITER.get() {
        // SAFETY: 输出串行化由 OUTPUT 保证，同一时刻只有一个 &mut。
        f(unsafe { &mut *w.0 });
    }
}

/// 输出到当前注册的 Write 设备，无换行
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::print::outs(|w| {
            let _ = core::fmt::Write::write_fmt(w, format_args!($($arg)*));
        });
    };
}

/// 输出到当前注册的 Write 设备，自动换行
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*));
    };
}
