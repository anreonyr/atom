// 陷阱 / 中断处理
//
// trap_vector 是硬件入口（#[unsafe(naked)]，无编译器序言/尾声），
// 它直接 call trap_handler_rust 再 mret 返回。

use crate::{plic, timer, uart};
use core::arch::asm;
use core::arch::naked_asm;

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn trap_vector() {
    naked_asm!(
        "call {handler}",
        "mret",
        handler = sym trap_handler_rust,
    );
}

#[no_mangle]
extern "C" fn trap_handler_rust() {
    let mcause = csr_read!(mcause);

    if mcause >> 63 == 1 {
        // 异步中断
        let irq = mcause & !(1 << 63);
        if irq == 7 {
            timer::handler();
            timer::set_timer(timer::TICKS_PER_SEC);
        } else if irq == 11 {
            // Machine External Interrupt → PLIC
            let source = plic::claim();
            if source == plic::UART0_IRQ {
                uart::UART.handle_rx();
            }
            plic::complete(source);
        } else {
            uart::UART.puts("[trap] unknown IRQ\n");
        }
    } else {
        // 同步异常
        uart::UART.puts("[trap] exception!\n");
    }
}
