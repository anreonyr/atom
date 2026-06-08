// 陷阱 / 中断处理
//
// trap_vector 是硬件入口（#[unsafe(naked)]，无编译器序言/尾声），
// 它直接 call trap_handler_rust 再 mret 返回。
//
// 中断分发逻辑（全部在 trap_handler_rust 内完成，无间接调用）：
//   mcause=3  (MSI) → CLINT.handle_soft_irq()
//   mcause=7  (MTI) → CLINT.handle_timer_irq()
//   mcause=11 (MEI) → PLIC.claim() → EXTERNAL_HANDLERS 匹配 → handler

use alloc::vec::Vec;
use core::arch::asm;
use core::arch::naked_asm;

use crate::drivers::{CLINT, PLIC};
use crate::hal::{InterruptController, IrqHandler};

// ── 外部中断处理器注册表 ─────────────────────────────────

/// 外部中断处理器列表（mcause=11 → PLIC），按 irq_number 匹配
static mut EXTERNAL_HANDLERS: Option<Vec<&'static dyn IrqHandler>> = None;

/// 初始化外部中断注册表（在 allocator 初始化后调用一次）
pub fn init_handlers() {
    unsafe {
        EXTERNAL_HANDLERS = Some(Vec::new());
    }
}

/// 注册外部中断处理器（mcause=11，经 PLIC 路由）
#[allow(static_mut_refs)]
pub fn register_irq(handler: &'static dyn IrqHandler) {
    unsafe {
        EXTERNAL_HANDLERS.as_mut().unwrap().push(handler);
    }
}

// ── 陷阱入口 ─────────────────────────────────────────────

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
        // ── 异步中断 ──────────────────────────────────
        let irq = mcause & !(1 << 63);
        match irq {
            3 => {
                // 机器软件中断 (MSI) — CLINT MSIP
                CLINT.handle_soft_irq();
            }
            7 => {
                // 机器定时器中断 (MTI) — CLINT MTIMECMP
                CLINT.handle_timer_irq();
            }
            11 => {
                // 机器外部中断 (MEI) → PLIC
                let source = PLIC.claim();
                if source != 0 {
                    unsafe {
                        if let Some(ref handlers) = EXTERNAL_HANDLERS {
                            for h in handlers {
                                if h.irq_number() == source {
                                    h.handle_irq();
                                    break;
                                }
                            }
                        }
                    }
                }
                PLIC.complete(source);
            }
            _ => {
                println!("[trap] unknown IRQ");
            }
        }
    } else {
        // ── 同步异常 ──────────────────────────────────
        println!("[trap] exception!");
    }
}
