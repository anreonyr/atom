// UART 硬件能力契约 — 串口设备能力 trait（hal = 全部硬件能力契约的集合）
//
// 纯硬件能力（零依赖）：驱动（driver/uart/）实现本 trait 暴露硬件操作。
// 服务集成（File / fmt::Write / InterruptHandler 适配 + 注册表 + sink 联动）
// 在 src/uart.rs 契约层——驱动代码中不出现 File 类型。

/// 串口设备能力 — 驱动实现的硬件接口。
///
/// 驱动（如 Uart16550/SifiveUart）实现本 trait 暴露硬件操作；
/// VFS 的 File / console 的 Write / 中断的 InterruptHandler 三个视图
/// 由 src/uart.rs 注册表层 blanket 提供，驱动代码中不出现 File 类型。
pub trait Uart: Send + Sync {
    /// 写入单字节（轮询 TX 就绪，锁外）。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 MMIO 区域已映射。
    unsafe fn write_byte(&self, c: u8);

    /// 非阻塞读取单字节——无数据就绪返回 `None`。
    fn read_byte(&self) -> Option<u8>;

    /// 中断号（PLIC 路由）。
    fn interrupt_number(&self) -> u32;

    /// 中断处理（RX 数据到达；当前为回显模式）。
    fn handle_interrupt(&self);

    /// 使能 RX 中断。
    fn enable_interrupt(&self);
}
