/// 外部中断控制器抽象
///
/// 管理通过 PLIC 路由的外部中断：使能/屏蔽、优先级、claim/complete。
pub trait InterruptController {
    fn init(&self);
    fn enable(&self, irq: u32);
    fn set_priority(&self, irq: u32, priority: u32);
    fn claim(&self) -> u32;
    fn complete(&self, irq: u32);
}
