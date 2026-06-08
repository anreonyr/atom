/// 中断处理器抽象
///
/// 所有会产生中断的设备统一实现此 trait。
pub trait IrqHandler {
    fn irq_number(&self) -> u32;
    fn handle_irq(&self);
    fn enable_irq(&self);
}
