// 驱动生命周期抽象
//
// 所有 MMIO 设备驱动实现此 trait，通过统一的 init() 入口完成初始化。
// 驱动在 init.rs 中按依赖顺序依次调用 init()。

/// 驱动初始化错误
#[derive(Debug)]
pub enum DriverError {
    /// 硬件未响应（读回值与预期不符）
    NotResponding,
    /// MMIO / 中断号资源冲突
    ResourceConflict,
    /// 前置依赖未初始化
    DependencyMissing,
    /// 其他初始化失败
    Other(&'static str),
}

/// 驱动生命周期抽象
///
/// 实现者通过 `init()` 完成所有硬件初始化，包括基础寄存器配置和中断路由。
/// 调用者（`init.rs`）负责按依赖顺序排列各驱动的 `init()` 调用。
pub trait Driver {
    /// 驱动名称，用于诊断/日志（如 `"ns16550a"`）。
    fn name(&self) -> &'static str;

    /// 初始化设备硬件。
    ///
    /// 调用时以下设施已就绪：
    /// - 分配器（堆分配可用）
    /// - MMU（Sv39 分页已启用）
    /// - 陷阱向量
    ///
    /// 调用时以下设施**可能尚未就绪**：
    /// - print!/println!/日志宏
    /// - 外部中断（全局中断尚未使能）
    /// - 其他驱动（按 init.rs 中的排列顺序确定可用性）
    fn init(&self) -> Result<(), DriverError>;
}
