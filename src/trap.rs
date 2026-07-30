// 陷阱 / 中断处理
//
// trap_vector 是硬件入口（#[unsafe(naked)]，无编译器序言/尾声），
// 它直接 call trap_handler 再 sret 返回。
//
// 中断分发逻辑（全部在 trap_handler 内完成，无间接调用）：
//   scause=1 (SSI) → 清除 sip.SSIP（架构行为，不依赖具体设备）
//   scause=5 (STI) → hal::timer::get() handle_interrupt
//   scause=9 (SEI) → hal::interrupt::get() claim → INTERRUPT_HANDLERS → complete

use alloc::vec::Vec;
use core::arch::naked_asm;

use crate::hal::csr::scause::{self, Scause};
use crate::hal::csr::{sepc, stvec};
use crate::hal::InterruptHandler;
use crate::lock::SpinLock;
use crate::scheduler;


/// 外部中断处理器表（scause=9 → PLIC），按中断号索引。
///
/// 引导期 `register_interrupt_handler` 时自动 resize 到 `interrupt_number + 1`，
/// 运行时 `trap_handler` 以 O(1) 直接定位。
static INTERRUPT_HANDLERS: SpinLock<Vec<Option<&'static dyn InterruptHandler>>> = SpinLock::new(Vec::new());

/// 初始化陷阱向量（在 allocator 初始化后调用一次）
pub unsafe fn init() {
    stvec::write(crate::trap::trap_vector as *const () as usize)
}

/// 注册外部中断处理器（scause=9，经 PLIC 路由，按中断号索引）
pub fn register_interrupt_handler(handler: &'static dyn InterruptHandler) {
    let interrupt = handler.interrupt_number() as usize;
    let mut table = INTERRUPT_HANDLERS.lock();
    if interrupt >= table.len() {
        table.resize(interrupt + 1, None);
    }
    table[interrupt] = Some(handler);
}

#[repr(C)]
pub struct TrapFrame {
    pub ra: usize,      // x1    offset 0
    pub sp: usize,      // x2    offset 8    ← 任务被打断前的原始 sp
    pub gp: usize,      // x3    offset 16
    pub tp: usize,      // x4    offset 24
    pub t0: usize,      // x5    offset 32
    pub t1: usize,      // x6    offset 40
    pub t2: usize,      // x7    offset 48
    pub s0: usize,      // x8    offset 56
    pub s1: usize,      // x9    offset 64
    pub a0: usize,      // x10   offset 72
    pub a1: usize,      // x11   offset 80
    pub a2: usize,      // x12   offset 88
    pub a3: usize,      // x13   offset 96
    pub a4: usize,      // x14   offset 104
    pub a5: usize,      // x15   offset 112
    pub a6: usize,      // x16   offset 120
    pub a7: usize,      // x17   offset 128
    pub s2: usize,      // x18   offset 136
    pub s3: usize,      // x19   offset 144
    pub s4: usize,      // x20   offset 152
    pub s5: usize,      // x21   offset 160
    pub s6: usize,      // x22   offset 168
    pub s7: usize,      // x23   offset 176
    pub s8: usize,      // x24   offset 184
    pub s9: usize,      // x25   offset 192
    pub s10: usize,     // x26   offset 200
    pub s11: usize,     // x27   offset 208
    pub t3: usize,      // x28   offset 216
    pub t4: usize,      // x29   offset 224
    pub t5: usize,      // x30   offset 232
    pub t6: usize,      // x31   offset 240
    pub sepc: usize,    // CSR   offset 248
    pub sstatus: usize, // CSR   offset 256
}


#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn trap_vector() {
    naked_asm!(
        "addi   sp, sp, -264",

        "sd ra, 0(sp)",
        "sd gp, 16(sp)",
        "sd tp, 24(sp)",
        "sd t0, 32(sp)",
        "sd t1, 40(sp)",
        "sd t2, 48(sp)",
        "sd s0, 56(sp)",
        "sd s1, 64(sp)",
        "sd a0, 72(sp)",
        "sd a1, 80(sp)",
        "sd a2, 88(sp)",
        "sd a3, 96(sp)",
        "sd a4, 104(sp)",
        "sd a5, 112(sp)",
        "sd a6, 120(sp)",
        "sd a7, 128(sp)",
        "sd s2, 136(sp)",
        "sd s3, 144(sp)",
        "sd s4, 152(sp)",
        "sd s5, 160(sp)",
        "sd s6, 168(sp)",
        "sd s7, 176(sp)",
        "sd s8, 184(sp)",
        "sd s9, 192(sp)",
        "sd s10, 200(sp)",
        "sd s11, 208(sp)",
        "sd t3, 216(sp)",
        "sd t4, 224(sp)",
        "sd t5, 232(sp)",
        "sd t6, 240(sp)",

        "addi   t0, sp, 264",
        "sd t0, 8(sp)",

        "csrr   t0, sepc",
        "sd t0, 248(sp)",
        "csrr   t0, sstatus",
        "sd t0, 256(sp)",

        "mv a0, sp",
        "call   {handler}",
        "mv sp, a0",

        "ld t0, 248(sp)",
        "csrw   sepc,   t0",
        "ld t0, 256(sp)",
        "csrw   sstatus,    t0",

        "ld ra, 0(sp)",
        "ld gp, 16(sp)",
        "ld tp, 24(sp)",
        "ld t0, 32(sp)",
        "ld t1, 40(sp)",
        "ld t2, 48(sp)",
        "ld s0, 56(sp)",
        "ld s1, 64(sp)",
        "ld a0, 72(sp)",
        "ld a1, 80(sp)",
        "ld a2, 88(sp)",
        "ld a3, 96(sp)",
        "ld a4, 104(sp)",
        "ld a5, 112(sp)",
        "ld a6, 120(sp)",
        "ld a7, 128(sp)",
        "ld s2, 136(sp)",
        "ld s3, 144(sp)",
        "ld s4, 152(sp)",
        "ld s5, 160(sp)",
        "ld s6, 168(sp)",
        "ld s7, 176(sp)",
        "ld s8, 184(sp)",
        "ld s9, 192(sp)",
        "ld s10, 200(sp)",
        "ld s11, 208(sp)",
        "ld t3, 216(sp)",
        "ld t4, 224(sp)",
        "ld t5, 232(sp)",
        "ld t6, 240(sp)",

        "ld sp, 8(sp)",

        "sret",

        handler = sym trap_handler,
    );
}

#[no_mangle]
extern "C" fn trap_handler(frame: *mut TrapFrame) -> usize {
    let scause = unsafe { scause::read() };

    if scause.contains(Scause::INTERRUPT) {
        // ── 异步中断 ──────────────────────────────────
        match scause.code() {
            1 => {
                // 监管者软件中断 (SSI) — 清除 SSIP 挂起位
                // SAFETY: sip.SSIP 清零是 RISC-V S-mode 架构行为
                debug!("IPI received");
                unsafe { core::arch::asm!("csrc sip, {}", in(reg) 1usize << 1) };
            }
            5 => {
                // 监管者定时器中断 (STI) → InternalInterrupt
                crate::hal::interrupt::get_internal().handle_timer();
                return scheduler::scheduler(frame);
            }
            9 => {
                // 监管者外部中断 (SEI) → ExternalInterrupt
                let ic = crate::hal::interrupt::get_external();
                let source = ic.claim();
                if source != 0 {
                    let table = INTERRUPT_HANDLERS.lock();
                    if let Some(Some(h)) = table.get(source as usize) {
                        h.handle_interrupt();
                    }
                }
                ic.complete(source);
            }
            _ => {
                warn!("unknown IRQ (scause={:#x})", scause);
            }
        }
    } else {
        // ── 同步异常 ──────────────────────────────────
        let code = scause.code();
        match code {
            12 | 13 | 15 => {
                // 缺页异常 — 委托给 mmu::fault 模块处理
                let fault = unsafe { crate::mmu::fault::PageFault::capture() };
                let guard = crate::mmu::KERNEL_SPACE.lock();
                let handled = match &*guard {
                    Some(ks) => crate::mmu::fault::handle_page_fault(&fault, ks),
                    None => false,
                };
                if !handled {
                    panic!("unhandled page fault: {:?}", fault);
                }
            }
            _ => {
                error!("exception! scause={:#x}, sepc={:#x}", scause, unsafe {
                    sepc::read()
                });
            }
        }
    }

    frame as usize
}
