/// 驱动子系统错误。
#[derive(Debug, Clone, Copy)]
pub enum DriverError {
    /// hub 中未找到指定驱动
    NotFound(&'static str),
    /// 驱动硬件初始化失败
    Init(&'static str),
    /// MMIO 映射失败
    MapFailed(&'static str),
}

pub trait Driver: Sized {
    fn init(&'static self) -> Result<(), DriverError>;
}
