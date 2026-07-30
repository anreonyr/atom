/// 硬件定时器抽象 — 提供单调递增的时间计数和可编程定时中断
///
/// RISC-V 平台上不同实现：CLINT、ACLINT mtimer、Sstc 等。
pub trait Timer: Send + Sync {
    /// 定时器频率（Hz），用于将 tick 数转换为时间单位
    fn frequency(&self) -> u64;
    /// 读取当前时间计数值
    fn read(&self) -> u64;
    /// 设置下次定时中断的绝对时间值
    fn next(&self, abs: u64);
    /// 定时器中断处理（默认行为：以 frequency 为间隔重装）
    fn handle_interrupt(&self) {
        self.next(self.read().wrapping_add(self.frequency()));
    }
}

use crate::lock::OnceLock;

static TIMER: OnceLock<&'static dyn Timer> = OnceLock::new();

/// 注册定时器实例（引导期调用一次）
pub fn register(t: &'static dyn Timer) {
    if TIMER.set(t).is_err() {
        panic!("timer already registered");
    }
}

/// 获取当前注册的定时器
pub fn get() -> &'static dyn Timer {
    match TIMER.get() {
        Some(&t) => t,
        None => panic!("timer not registered"),
    }
}
