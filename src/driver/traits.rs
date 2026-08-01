// 驱动抽象 — 数据式 Driver trait + 错误码
//
// 对应 Linux `struct device_driver`：一个 Driver 是"数据 + probe 回调"，
// 认识一类 compatible（id_table），由 hub 匹配设备后调用 probe()。
// 与旧的 `Driver::init(&'static self)` 伪抽象不同——三个设备的 init 行为
// 各不相同，无法统一；而 compatibles + probe 是驱动真正共享的结构。

use crate::driver::device::Device;

/// 驱动子系统错误。
#[derive(Debug, Clone, Copy)]
pub enum DriverError {
    /// probe 时依赖尚未就绪，需延后重试（Linux `-EPROBE_DEFER` 语义）。
    ///
    /// hub 的 `probe_all()` 会对返回此错误的设备自动重试，直到所有设备
    /// 均 probe 成功或出现 [`DriverError::Stuck`]。
    Deferred,
    /// 驱动硬件初始化失败。
    #[allow(dead_code)] // payload 为错误上下文（Debug 输出），暂未按字段读取
    Init(&'static str),
    /// MMIO 映射失败。
    #[allow(dead_code)]
    MapFailed(&'static str),
    /// deferred 循环无进展——存在依赖环，或遗漏了某型号的驱动。
    Stuck,
}

/// 设备驱动 — 认识一类 compatible，负责匹配成功后初始化设备。
///
/// 每个驱动模块（如 `serial/uart16550.rs`）导出一个静态实例，
/// 经角色目录（`serial`/`controller`）的 `DRIVERS` 常量汇总进 hub。
/// 一个 Driver 对应一种设备型号；同类型的不同型号（如 16550 / PL011）
/// 是多个 Driver，各自声明自己的 `compatibles()`。
pub trait Driver: Send + Sync {
    /// 驱动名（如 "uart16550"、"plic"、"clint"）。
    ///
    /// 由 [`Hub::bound_devices`](crate::driver::hub::Hub::bound_devices) 消费
    /// （boot 日志逐设备报告绑定结果）。
    fn name(&self) -> &'static str;

    /// 认识的 compatible 列表（id_table，如 `&["ns16550a"]`）。
    fn compatibles(&self) -> &'static [&'static str];

    /// 设备匹配成功后初始化硬件，并把实例挂到设备上（`dev.set_instance`）。
    ///
    /// probe 的内容由驱动自含：MMIO 映射、构造实例、硬件初始化、
    /// 中断注册。
    ///
    /// # Deferred 语义
    ///
    /// 依赖尚未就绪时返回 [`DriverError::Deferred`]，hub 会延后到下一轮重试。
    /// **probe 必须保证返回 Deferred 之前不产生任何副作用**（依赖检查放最前），
    /// 否则重试会重复映射/构造，导致 `AlreadyMapped` 等错误。
    fn probe(&self, dev: &Device) -> Result<(), DriverError>;
}
