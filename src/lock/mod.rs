// 锁模块 — SpinLock + OnceLock
//
// 提供内核同步原语：
//   - SpinLock<T>: 中断安全自旋锁（关闭 SIE 防止重入死锁）
//   - OnceLock<T>: 一次性初始化容器（写入一次，只读多次，读取无锁）

mod once;
mod spin;

pub use once::OnceLock;
pub use spin::SpinLock;
