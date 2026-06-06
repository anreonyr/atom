// PLIC (Platform-Level Interrupt Controller)
//
// QEMU virt: PLIC_BASE = 0x0C00_0000
//
// 寄存器布局：
//   BASE + source * 4           → 中断优先级
//   BASE + 0x001000 + N * 4     → 挂起位
//   BASE + 0x002000 + ctx * 0x80 + N * 4  → 使能位
//   BASE + 0x200000 + ctx * 0x1000        → 优先级阈值
//   BASE + 0x200004 + ctx * 0x1000        → Claim / Complete

const PLIC_BASE: usize = 0x0C00_0000;

/// QEMU virt 上的中断源编号
pub const UART0_IRQ: u32 = 10;

const HART_ID: usize = 0; // hart 0, M-mode

/// 初始化 PLIC：设 UART0 优先级、阈值归零、使能中断源
pub fn init() {
    unsafe {
        // 1) 设置 UART0 优先级（>0 才有效）
        set_priority(UART0_IRQ, 1);

        // 2) 设置当前上下文阈值 = 0（接收所有优先级）
        let thresh = (PLIC_BASE + 0x200000 + HART_ID * 0x1000) as *mut u32;
        thresh.write_volatile(0);

        // 3) 使能 UART0 在当前上下文
        enable(HART_ID, UART0_IRQ);
    }
}

/// 设置中断源优先级
unsafe fn set_priority(source: u32, priority: u32) {
    let p = (PLIC_BASE + source as usize * 4) as *mut u32;
    p.write_volatile(priority);
}

/// 在指定上下文上使能中断源
unsafe fn enable(context: usize, source: u32) {
    let word = (source / 32) as usize;
    let bit = source % 32;
    let addr = (PLIC_BASE + 0x002000 + context * 0x80 + word * 4) as *mut u32;
    addr.write_volatile(addr.read_volatile() | 1 << bit);
}

/// Claim：读取当前上下文的中断源编号（0 = 无中断）
pub fn claim() -> u32 {
    let claim = (PLIC_BASE + 0x200004 + HART_ID * 0x1000) as *const u32;
    unsafe { claim.read_volatile() }
}

/// Complete：通知 PLIC 该中断已处理完毕
pub fn complete(irq: u32) {
    let comp = (PLIC_BASE + 0x200004 + HART_ID * 0x1000) as *mut u32;
    unsafe { comp.write_volatile(irq) }
}
