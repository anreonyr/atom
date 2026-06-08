// print!/println! — 全局输出宏
//
// 从设备注册中心获取 `&dyn Write` 实例输出。
// 通过 SpinLock 保证多核下输出不交错。
//
// 为了避免 invalid_reference_casting，在 init() 时将引用转成裸指针
// 存入 WRITER，此后 `outs()` 中只从裸指针拿 `&mut`，编译器不追踪源头。

use crate::lock::SpinLock;

// 以裸指针存储，避免每次从 &T 转 &mut
static WRITER: SpinLock<Option<*mut dyn core::fmt::Write>> = SpinLock::new(None);

// 输出串行化锁
static OUTPUT: SpinLock<()> = SpinLock::new(());

/// 初始化输出设备（在 device::register 之后调用）
#[allow(static_mut_refs)]
pub fn init() {
    let w = crate::drivers::device::get::<dyn core::fmt::Write>();
    // 一次性的 &T → *mut 转换
    let w = w as *const dyn core::fmt::Write as *mut dyn core::fmt::Write;
    WRITER.lock(|cell| {
        *cell = Some(w);
    });
}

/// 在锁保护下执行输出闭包
pub fn outs<F>(f: F)
where
    F: FnOnce(&mut dyn core::fmt::Write),
{
    OUTPUT.lock(|_| {
        WRITER.lock(|cell| {
            if let Some(w) = *cell {
                f(unsafe { &mut *w });
            }
        });
    });
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
