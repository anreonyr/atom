// 陷阱 / 中断处理
//
// trap_vector 是硬件入口（#[unsafe(naked)]，无编译器序言/尾声），
// 它直接 call trap_handler 再 sret 返回。
//
// 中断分发逻辑（全部在 trap_handler 内完成，无间接调用）：
//   scause=1 (SSI) → 清除 sip.SSIP（架构行为，不依赖具体设备）
//   scause=5 (STI) → clock::tick::on_timer()（重装 + jiffies）→ scheduler
//   scause=9 (SEI) → hal::interrupt::get() claim → INTERRUPT_HANDLERS → complete
// 同步异常：scause=8 (ecall from U-mode) → envcall::dispatch（骨架）

use alloc::vec::Vec;
use core::arch::naked_asm;

use crate::context::TrapFrame;
use crate::hal::InterruptHandler;
use crate::hal::csr::scause::{self, Scause};
use crate::hal::csr::{sepc, stval, stvec};
use crate::lock::SpinLock;
use crate::schedule;
use crate::{debug, error, warn};

/// 外部中断处理器表（scause=9 → PLIC），按中断号索引。
///
/// 引导期 `register_interrupt_handler` 时自动 resize 到 `interrupt_number + 1`，
/// 运行时 `trap_handler` 以 O(1) 直接定位。
static INTERRUPT_HANDLERS: SpinLock<Vec<Option<&'static dyn InterruptHandler>>> =
    SpinLock::new(Vec::new());

/// 初始化陷阱向量（在 allocator 初始化后调用一次）
/// # Safety
pub unsafe fn init() {
    unsafe { stvec::write(crate::trap::trap_vector as *const () as usize) }
}

/// Maximum PLIC interrupt source number this kernel supports.
const MAX_INTERRUPTS: usize = 256;

/// Register an external interrupt handler (scause=9, routed through PLIC, indexed by IRQ number).
///
/// # Panics
///
/// Panics if `interrupt_number` exceeds [`MAX_INTERRUPTS`], or if another handler
/// is already registered for the same IRQ (no shared-IRQ support — 重复注册
/// 视为编程错误，静默覆盖会掩盖驱动间冲突，panic 让 boot 期立即暴露).
pub fn register_interrupt_handler(handler: &'static dyn InterruptHandler) {
    let interrupt = handler.interrupt_number() as usize;
    assert!(
        interrupt <= MAX_INTERRUPTS,
        "interrupt number {} exceeds MAX_INTERRUPTS ({})",
        interrupt,
        MAX_INTERRUPTS
    );
    let mut table = INTERRUPT_HANDLERS.lock();
    if interrupt >= table.len() {
        table.resize(interrupt + 1, None);
    }
    if let Some(old) = table[interrupt] {
        panic!(
            "interrupt handler already registered for IRQ {} (old: {}, new: {})",
            interrupt,
            core::any::type_name_of_val(old),
            core::any::type_name_of_val(handler),
        );
    }
    table[interrupt] = Some(handler);
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
/// # Safety
pub unsafe extern "C" fn trap_vector() {
    naked_asm!(
        //    保存帧前检查：sp 若落在守护页 [BASE-4K, BASE)，说明任务栈已被写穿
        //    （递归压栈溢出）——此时保存帧本身会再次触发缺页 → 嵌套下降 livelock。
        //    改走专用路径 trap_stack_corrupt（UMode terminate / kernel panic）。
        //    boot 栈（DRAM 顶，sp < BASE-4K）不受影响，走正常路径。
        //    检查用寄存器不污染任务状态：csrrw 把任务原始 t0 换入 sscratch，
        //    t0 随后被 li 覆盖作比较寄存器；t1 全程不触碰。正常路径 2: 处
        //    csrr 取回原始 t0 再保存帧——被抢占任务恢复的 t0/t1 均为原值。
        //    破坏路径不恢复 t0（任务 terminate/panic，无恢复语义）；sscratch
        //    残留值由下一次 trap 入口的 csrrw 覆盖（单 hart + non-nesting）。
        "csrrw  t0, sscratch, t0", // sscratch ← 任务原始 t0；t0 获残留值（马上覆盖）
        "li     t0, {base}",
        "bgeu   sp, t0, 2f",       // sp >= BASE：任务栈内，正常
        "li     t0, {guard}",
        "bltu   sp, t0, 2f",       // sp < BASE-4K：boot 栈等，正常
        "la     sp, _trap_stack_top",
        "call   {corrupt}",        // 任务不恢复，t0 脏值无妨
        "j      3f",
        "2:",
        "csrr   t0, sscratch",     // 恢复任务原始 t0（残留值下次入口被覆盖）
        //    置 SUM（sstatus.bit18）：S-mode 需要写**任务栈**保存帧——UMode 任务
        //    的栈是 U 页，SUM=0 时第一条 sd 指令就缺页，必须在保存帧前置位。
        //    用 csrrw 借 t1 中转（csrsi 立即数仅 5 位**掩码值**，无法表达 bit 18）：
        //    t1 换入 sscratch → 加载 1<<18 → csrs → 换回任务 t1；sscratch 残留
        //    sum 值由下一次 trap 入口的 csrrw 覆盖（单 hart + non-nesting）。
        //    置位后 trap 全程（保存帧 / handler / 调度器 / 恢复 next 帧）SUM=1；
        //    sret 用帧里 sstatus 恢复——帧保存的是置位后的值，故任务恢复后
        //    SUM 恒 1：用户态 SUM 无意义（仅 S-mode 检查），SMode 任务空间无
        //    U 页映射，均无影响。
        "csrrw  t1, sscratch, t1",
        "li     t1, {sum}",
        "csrs   sstatus, t1",
        "csrrw  t1, sscratch, t1",
        //    在**任务栈**上保存帧（sp-relative）。帧必须留在任务栈：
        //    调度器靠帧指针在任务间切换，per-task 栈窗口保证各任务帧互不覆盖。
        //    帧槽尺寸与所有字段偏移引用 context::FRAME_* 编译期常量，
        //    与 TrapFrame 布局永远一致（offset_of! 派生，杜绝手工同步漂移）。
        "addi   sp, sp, -{frame_size}",
        "sd ra, {off_ra}(sp)",
        "sd gp, {off_gp}(sp)",
        "sd tp, {off_tp}(sp)",
        "sd t0, {off_t0}(sp)",
        "sd t1, {off_t1}(sp)",
        "sd t2, {off_t2}(sp)",
        "sd s0, {off_s0}(sp)",
        "sd s1, {off_s1}(sp)",
        "sd a0, {off_a0}(sp)",
        "sd a1, {off_a1}(sp)",
        "sd a2, {off_a2}(sp)",
        "sd a3, {off_a3}(sp)",
        "sd a4, {off_a4}(sp)",
        "sd a5, {off_a5}(sp)",
        "sd a6, {off_a6}(sp)",
        "sd a7, {off_a7}(sp)",
        "sd s2, {off_s2}(sp)",
        "sd s3, {off_s3}(sp)",
        "sd s4, {off_s4}(sp)",
        "sd s5, {off_s5}(sp)",
        "sd s6, {off_s6}(sp)",
        "sd s7, {off_s7}(sp)",
        "sd s8, {off_s8}(sp)",
        "sd s9, {off_s9}(sp)",
        "sd s10, {off_s10}(sp)",
        "sd s11, {off_s11}(sp)",
        "sd t3, {off_t3}(sp)",
        "sd t4, {off_t4}(sp)",
        "sd t5, {off_t5}(sp)",
        "sd t6, {off_t6}(sp)",

        "addi   t0, sp, {frame_size}",
        "sd t0, {off_sp}(sp)",

        "csrr   t0, sepc",
        "sd t0, {off_sepc}(sp)",
        "csrr   t0, sstatus",
        "sd t0, {off_sstatus}(sp)",

        //    切到专用 trap 栈（恒等区，任何地址空间下都有效）。
        //    switch_space 后当前任务栈 VA 会别名到新任务栈，trap_handler/
        //    scheduler 绝不能跑在当前任务栈上。a0 保留帧地址传给 handler。
        "addi   a0, sp, 0",
        "la     sp, _trap_stack_top",
        "call   {handler}",
        "3:",                        // 与栈破坏路径汇合（a0 = 下一任务帧）
        //   handler 返回 a0 = 下一任务帧地址；用 t0 作基址恢复
        "mv     t0, a0",
        "ld     t1, {off_sepc}(t0)",
        "csrw   sepc, t1",
        "ld     t1, {off_sstatus}(t0)",
        "csrw   sstatus, t1",

        "ld ra, {off_ra}(t0)",
        "ld gp, {off_gp}(t0)",
        "ld tp, {off_tp}(t0)",
        "ld t1, {off_t1}(t0)",
        "ld t2, {off_t2}(t0)",
        "ld s0, {off_s0}(t0)",
        "ld s1, {off_s1}(t0)",
        "ld a0, {off_a0}(t0)",
        "ld a1, {off_a1}(t0)",
        "ld a2, {off_a2}(t0)",
        "ld a3, {off_a3}(t0)",
        "ld a4, {off_a4}(t0)",
        "ld a5, {off_a5}(t0)",
        "ld a6, {off_a6}(t0)",
        "ld a7, {off_a7}(t0)",
        "ld s2, {off_s2}(t0)",
        "ld s3, {off_s3}(t0)",
        "ld s4, {off_s4}(t0)",
        "ld s5, {off_s5}(t0)",
        "ld s6, {off_s6}(t0)",
        "ld s7, {off_s7}(t0)",
        "ld s8, {off_s8}(t0)",
        "ld s9, {off_s9}(t0)",
        "ld s10, {off_s10}(t0)",
        "ld s11, {off_s11}(t0)",
        "ld t3, {off_t3}(t0)",
        "ld t4, {off_t4}(t0)",
        "ld t5, {off_t5}(t0)",
        "ld t6, {off_t6}(t0)",

        "ld sp, {off_sp}(t0)",
        "ld t0, {off_t0}(t0)",

        "sret",

        handler = sym trap_handler,
        corrupt = sym trap_stack_corrupt,
        sum = const 1usize << 18, // sstatus.SUM — 允许 S-mode 访问 U 页（任务栈）
        base = const crate::memory::TASK_STACK_BASE,
        guard = const crate::memory::TASK_STACK_BASE - crate::memory::PAGE_SIZE,
        frame_size = const crate::context::FRAME_SIZE,
        off_ra = const crate::context::FRAME_OFF_RA,
        off_sp = const crate::context::FRAME_OFF_SP,
        off_gp = const crate::context::FRAME_OFF_GP,
        off_tp = const crate::context::FRAME_OFF_TP,
        off_t0 = const crate::context::FRAME_OFF_T0,
        off_t1 = const crate::context::FRAME_OFF_T1,
        off_t2 = const crate::context::FRAME_OFF_T2,
        off_s0 = const crate::context::FRAME_OFF_S0,
        off_s1 = const crate::context::FRAME_OFF_S1,
        off_a0 = const crate::context::FRAME_OFF_A0,
        off_a1 = const crate::context::FRAME_OFF_A1,
        off_a2 = const crate::context::FRAME_OFF_A2,
        off_a3 = const crate::context::FRAME_OFF_A3,
        off_a4 = const crate::context::FRAME_OFF_A4,
        off_a5 = const crate::context::FRAME_OFF_A5,
        off_a6 = const crate::context::FRAME_OFF_A6,
        off_a7 = const crate::context::FRAME_OFF_A7,
        off_s2 = const crate::context::FRAME_OFF_S2,
        off_s3 = const crate::context::FRAME_OFF_S3,
        off_s4 = const crate::context::FRAME_OFF_S4,
        off_s5 = const crate::context::FRAME_OFF_S5,
        off_s6 = const crate::context::FRAME_OFF_S6,
        off_s7 = const crate::context::FRAME_OFF_S7,
        off_s8 = const crate::context::FRAME_OFF_S8,
        off_s9 = const crate::context::FRAME_OFF_S9,
        off_s10 = const crate::context::FRAME_OFF_S10,
        off_s11 = const crate::context::FRAME_OFF_S11,
        off_t3 = const crate::context::FRAME_OFF_T3,
        off_t4 = const crate::context::FRAME_OFF_T4,
        off_t5 = const crate::context::FRAME_OFF_T5,
        off_t6 = const crate::context::FRAME_OFF_T6,
        off_sepc = const crate::context::FRAME_OFF_SEPC,
        off_sstatus = const crate::context::FRAME_OFF_SSTATUS,
    );
}

#[unsafe(no_mangle)]
extern "C" fn trap_handler(frame: *mut TrapFrame) -> usize {
    let scause = unsafe { scause::read() };

    if scause.contains(Scause::INTERRUPT) {
        // ── 异步中断 ──────────────────────────────────
        match scause.code() {
            1 => {
                // 监管者软件中断 (SSI) — 清 SSIP 挂起位（架构行为），随后重排
                // 调度：yield() 置位触发 self-IPI，这里立即让出当前任务
                // （round-robin 重排到队尾，不等 10ms tick）。
                // SAFETY: sip.SSIP 清零是 RISC-V S-mode 架构行为
                debug!("IPI received");
                unsafe { crate::hal::csr::sip::clear(crate::hal::csr::sip::Sip::SSIP) };
                return schedule::scheduler(frame);
            }
            5 => {
                // 监管者定时器中断 (STI) → clock tick 账目（重装 + jiffies），
                // 再编排调度——tick 归 clock，调度归 trap（依赖单向：
                // trap → clock + scheduler，clock 不依赖 scheduler）
                if !crate::clock::on_timer() {
                    return frame as usize;
                }
                return schedule::scheduler(frame);
            }
            9 => {
                // 监管者外部中断 (SEI) → ExternalInterrupt
                let Some(ic) = crate::hal::interrupt::get_external() else {
                    warn!("external interrupt before controller registered");
                    return frame as usize;
                };
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
            8 => {
                // U-mode 环境调用（ecall from U-mode）→ envcall 分发。
                // scause=8 只可能来自 U-mode 任务：S-mode 自身的 ecall 是
                // scause=9，直接陷入 M-mode OpenSBI，不经 trap_handler。
                // 参数/号取自已保存帧：a7=调用号，a0..a5=参数。分发结果：
                //   Ret(v) → v 写回 frame.a0；sepc += 4 跳过 ecall 指令——
                //     否则 sret 重试同一 ecall → livelock。例外：输入等待
                //     （read 缓冲空，dispatch 置 WaitRead）时**不加** sepc——
                //     sret 重放 ecall 是等待机制本身（trap 上下文 SIE=0 不能
                //     wfi，用户态忙转等 tick 抢占 park / 字符到达），读到数据
                //     后 dispatch 复位标记，此处 +4。
                //   Terminate(next) → exit syscall 已终止当前任务并调度下一
                //     任务，next 为下一 TrapFrame——直接恢复，不再写 a0。
                let args = [
                    unsafe { (*frame).a0 },
                    unsafe { (*frame).a1 },
                    unsafe { (*frame).a2 },
                    unsafe { (*frame).a3 },
                    unsafe { (*frame).a4 },
                    unsafe { (*frame).a5 },
                ];
                match crate::runtime::envcall::dispatch(frame, unsafe { (*frame).a7 }, args) {
                    crate::runtime::envcall::DispatchResult::Ret(v) => unsafe {
                        (*frame).a0 = v;
                        if !crate::schedule::is_input_waiting() {
                            (*frame).sepc = (*frame).sepc.wrapping_add(4);
                        }
                    },
                    crate::runtime::envcall::DispatchResult::Terminate(next) => return next,
                }
            }
            12 | 13 | 15 => {
                // 缺页异常 — 委托给 memory::fault 模块处理
                let fault = unsafe { crate::memory::fault::PageFault::capture() };

                // 按当前任务所属地址空间路由：None = 内核空间。
                // 活动空间由调度器的 CURRENT 推导（单一事实来源）。
                let cur = crate::schedule::current_space();
                let handled = match cur {
                    // SAFETY: CURRENT 持有该空间所有权（Arc），trap 期间不回收；
                    // 单 hart 关中断，调度器不会并发移除当前任务空间。
                    // 共享借用：缺页填充（map）经 RefCell 内部可变（空间可能
                    // 被多线程 Arc 共享）。
                    Some(sp) => {
                        crate::memory::fault::handle_page_fault(&fault, unsafe { sp.as_ref() })
                    }
                    None => match crate::memory::space::kernel_space().as_ref() {
                        Some(ks) => crate::memory::fault::handle_page_fault(&fault, ks),
                        None => false,
                    },
                };

                if !handled {
                    if crate::schedule::current_is_umode() {
                        // 用户任务未处理缺页 → 终止当前任务（等效于 SIGSEGV）。
                        // 栈守护页缺页（溢出被拦截在此）也走这条路径。
                        warn!("unhandled page fault in user task — terminating it");
                        return crate::schedule::terminate_current(frame, -1);
                    }
                    // 内核任务 / 空闲 / boot 未处理缺页：内核 bug → panic
                    panic!("unhandled page fault in kernel context: {:?}", fault);
                }
            }
            _ => {
                let sepc_val = unsafe { sepc::read() };
                error!("exception! scause={:#x}, sepc={:#x}", scause, sepc_val);
                if crate::schedule::current_is_umode() {
                    // 用户任务同步异常（非法指令/断点/ecall 等）→ 终止任务。
                    // 不再原样恢复同一帧（否则 sret 重试同一条指令 → livelock）。
                    warn!("unhandled synchronous exception in user task — terminating it");
                    return crate::schedule::terminate_current(frame, -1);
                }
                // 内核任务 / 空闲 / boot：内核 bug → panic（可诊断崩溃）
                panic!(
                    "unhandled synchronous exception: scause={:#x}, sepc={:#x}",
                    scause, sepc_val
                );
            }
        }
    }

    frame as usize
}

/// 栈破坏专用路径 — trap_vector 检测到 sp 落在守护页（递归压栈溢出）时调用。
///
/// 此时任务帧无法安全保存（保存会再触发缺页 → 嵌套下降 livelock），不保存帧：
/// User 任务 → [`terminate_current`]（null 帧仅标记 Zombie，dispatch 下一任务）；
/// 内核/空闲/boot → panic（栈破坏不可恢复）。
#[unsafe(no_mangle)]
extern "C" fn trap_stack_corrupt() -> usize {
    let scause_val = unsafe { scause::read() };
    let sepc_val = unsafe { sepc::read() };
    let stval_val = unsafe { stval::read() };
    error!("corrupted task stack: sp crossed guard page (stack overflow)");
    error!(
        "  scause={:#x} sepc={:#x} stval={:#x}",
        scause_val, sepc_val, stval_val
    );
    if crate::schedule::current_is_umode() {
        warn!("terminating user task after stack overflow");
        crate::schedule::terminate_current(core::ptr::null_mut(), -1)
    } else {
        panic!("corrupted kernel task stack (stack overflow)");
    }
}
