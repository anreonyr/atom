// 驱动生命周期抽象
//
// 所有 MMIO 设备驱动实现此 trait，通过统一的 init() 入口完成硬件初始化。
// probe() 负责从 DTB 发现缓冲区创建设备实例。

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
/// 实现者通过 `init()` 完成所有硬件初始化。
/// `probe()` 从 DTB 设备节点创建设备实例（引导期调用）。
/// `compatible()` 返回 DTB matching 字符串。
pub trait Driver: Sized {
    /// DTB compatible 字符串（如 `"ns16550a"`）。
    fn compatible() -> &'static str;

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
    /// - 其他驱动（按 `probe_all` 中的排列顺序确定可用性）
    fn init(&self) -> Result<(), DriverError>;

    /// 从 DTB 设备节点创建设备实例（引导期调用一次）。
    ///
    /// 负责创建静态实例（OnceLock），不接触硬件硬件——`init()` 阶段再做硬件配置。
    fn probe(dev: &crate::platform::DeviceNode);
}
