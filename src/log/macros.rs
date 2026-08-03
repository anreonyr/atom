// 日志宏 — 五级日志宏 + 条件日志
//
// `#[macro_export]` 全部导出到 crate root；宏内经 `$crate::log::` 引用
// mod.rs 重新导出的项（LogLevel/COMPILE_MAX_LEVEL/_log），与宏定义所在文件无关。

/// 条件日志（内部使用，通过具体级别宏调用）
///
/// 编译期常量比较使编译器在低级别构建中优化掉高于 COMPILE_MAX_LEVEL 的调用。
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        if ($level as u8) <= ($crate::log::COMPILE_MAX_LEVEL as u8) {
            $crate::log::_log(
                $level,
                format_args!($($arg)*),
                module_path!(),
                file!(),
                line!(),
            );
        }
    }};
}

/// 错误日志——最高严重级别
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Error, $($arg)*) };
}

/// 警告日志
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Warn, $($arg)*) };
}

/// 信息日志（默认运行时可见）
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Info, $($arg)*) };
}

/// 调试日志（默认运行时不可见，需 set_max_level(Debug) 启用）
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Debug, $($arg)*) };
}

/// 跟踪日志——最低级别，最详细
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => { $crate::log!($crate::log::LogLevel::Trace, $($arg)*) };
}

