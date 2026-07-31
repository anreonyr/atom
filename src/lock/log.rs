// 锁调试日志 — M-mode SBI 直写的无锁日志通道
//
// 供锁的获取/释放调试使用。经 mprint（SBI ecall）输出，绕开日志系统的
// ring buffer 与输出 writer 锁，因此可安全用于日志系统自身的临界区
// （锁内打日志不会递归）。
//
// 由调用点的 `#[cfg(feature = "...")]` 按锁类型控制编译
// （spin-trace / bare-trace / rw-trace / rel-trace，或聚合 lock-trace）。
// feature 未开时调用整行移除、零开销，独立于 debug/release 构建。

/// 无锁调试日志 — M-mode SBI 直写控制台。
///
/// 格式：`[DEBUG] (module_path) msg`。无时间戳/颜色（SBI 通道不依赖
/// 日志系统的格式化设施）。由调用点的 `#[cfg(feature = "...")]` 控制。
#[macro_export]
macro_rules! lock_debug {
    ($($arg:tt)*) => {
        $crate::mprintln!(
            "[DEBUG] ({}): {}",
            module_path!(),
            format_args!($($arg)*)
        );
    };
}
