// 陷阱 / 中断处理
//
// trap_vector 是硬件入口（#[unsafe(naked)]，无编译器序言/尾声），
// 它直接 call trap_handler_rust 再 mret 返回。

use crate::{timer, uart};
use core::arch::asm;
use core::arch::naked_asm;

// ── 陷阱入口 ──────────────────────────────────────────────
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn trap_vector() {
    naked_asm!(
        "call {handler}",
        "mret",
        handler = sym trap_handler_rust,
    );
}

// ── 陷阱分发 ──────────────────────────────────────────────
#[no_mangle]
extern "C" fn trap_handler_rust() {
    let mcause = csr_read!(mcause);

    if mcause >> 63 == 1 {
        // 异步中断
        let irq = mcause & !(1 << 63);
        if irq == 7 {
            timer::handler();
            timer::set_timer(timer::TICKS_PER_SEC);
        } else {
            uart::puts("[trap] unknown IRQ\n");
        }
    } else {
        // 同步异常
        uart::puts("[trap] exception!\n");
    }
}
