// 字节通道硬件能力契约 — 通用「字节收发 + 中断」设备能力（hal = 全部硬件能力契约的集合）
//
// 纯硬件能力（零依赖）：驱动（driver/uart/）实现本 trait 暴露硬件操作。
// 服务集成（File / fmt::Write / InterruptHandler 适配 + 注册表）在
// console/console.rs 终端核心——驱动代码中不出现 File 类型。
//
// 命名说明：本契约描述「字节收发 + 中断」的通用设备能力（不限于 UART——
// virtio-console 等终端设备同样实现它），故名 ByteChannel；与 VFS 的
// `InodeType::ByteDevice`（devfs 节点类型，见 file/vfs/inode.rs）语义不同。

/// 字节通道设备能力 — 驱动实现的硬件接口（纯原子操作，Linux `uart_ops` 对应物）。
///
/// 驱动（如 Uart16550/SifiveUart）实现本 trait 暴露硬件操作；
/// VFS 的 File / console 的 Write / 中断的 InputHandler 三个视图由
/// console/console.rs 终端核心构造（中断字符搬运 + 回显 + 缓冲 + 唤醒均归
/// 服务层，对应 Linux：驱动只做原子收发，终端语义在 tty core）——
/// 驱动代码中不出现 File 等类型。
pub trait ByteChannel: Send + Sync {
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

    /// 使能 RX 中断。
    fn enable_interrupt(&self);
}
