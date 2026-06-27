/// 定时器控制器抽象
///
/// 提供基于硬件定时器的周期性 tick 能力。
pub trait Timer {
    /// 返回定时器时钟频率 (Hz)，用于计算 tick 间隔。
    fn ticks_per_sec(&self) -> u64;
    fn set_timer(&self, interval: u64);
    fn read_mtime(&self) -> u64;
}
