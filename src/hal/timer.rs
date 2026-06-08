/// 定时器控制器抽象
///
/// 提供基于硬件定时器的周期性 tick 能力。
pub trait Timer {
    const TICKS_PER_SEC: u64;
    fn set_timer(&self, interval: u64);
    fn read_mtime(&self) -> u64;
}
