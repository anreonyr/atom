// 阻塞-唤醒状态机（scheduler 子模块）
//
// sleep() 把当前任务置 Blocked 后 wfi 等 tick；调度核心经 wake_task 唤醒
// 到期任务（置 Ready + sepc 重置到 resume_after_sleep）。另含 mtime 时刻
// 读取（now_ticks）与跨任务物理帧访问辅助（frame_phys / in_dram）。

use core::mem::size_of;
use core::time::Duration;

use crate::{context::TrapFrame, debug};

use super::task::{Task, TaskState, CURRENT};

/// 当前时刻（mtime 刻度，来自 CLINT/`time` CSR）。
pub(crate) fn now_ticks() -> u64 {
    crate::hal::interrupt::get_internal()
        .map(|ii| ii.read())
        .unwrap_or(0)
}

/// 任务 TrapFrame 的物理地址。
///
/// 唤醒等"在其它任务空间激活时"的场合，任务自己的帧 VA 在当前空间
/// 不可见（会被静默映射到当前任务的栈顶帧），必须经物理地址访问——所有
/// 物理 DRAM 恒为各空间 identity 映射（`from_kernel` 全量复制 L2[2]），
/// 物理写在任何活动空间下都正确。空闲任务（space=None）栈在 boot 栈上
/// （identity，VA==PA），直接用它存的 frame 指针。
fn frame_phys(t: &Task) -> *mut TrapFrame {
    if t.space.is_some() {
        (t.stack as usize + t.stack_size - size_of::<TrapFrame>()) as *mut TrapFrame
    } else {
        t.frame
    }
}

/// 地址是否落在 DRAM 恒等映射区（跨任务物理访问的前提）。
///
/// 只在 `debug_assert!` 里引用；release 下断言折叠为 `if false` 分支，但
/// 该分支仍要解析本名称，故定义恒保留（无调用点时压掉 dead_code 警告）。
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) fn in_dram(addr: usize) -> bool {
    let cfg = crate::platform::get();
    (cfg.dram_base..cfg.dram_base + cfg.dram_size).contains(&addr)
}

/// 唤醒一个 Blocked 任务：置 Ready 并把 sepc 重置到恢复点。
///
/// **不能对保存的 sepc 做 `+=4`**：`wfi` 是 hint，若中断在 `csrs SIE` 时已挂起，
/// 它直接完成（不阻塞），trap 落在 wfi 的下一条指令——保存的 sepc 可能是
/// wfi 或其后的任意指令。无条件重置到 [`resume_after_sleep`]（仅含 `ret`）
/// 对"阻塞"与"立即返回"两种情形都正确，且与指令宽度（压缩指令）无关。
pub(crate) fn wake_task(t: &mut Task) {
    t.state = TaskState::Ready;
    // 校验帧物理地址在 DRAM——跨任务物理写（frame_phys）的前提，把"写错
    // 内存"提前到 wake 当场而非事后崩在调度目标上。
    debug_assert!(
        in_dram(frame_phys(t) as usize),
        "wake: frame {:#x} outside DRAM — cross-task physical write would hit garbage",
        frame_phys(t) as usize,
    );
    // SAFETY: 任务已 park 且空间未激活，其帧 VA 在当前空间不可见；经
    // 物理地址写（frame_phys，identity 映射下任意活动空间可见）。
    unsafe {
        (*frame_phys(t)).sepc = t.resume_sepc;
    }
    debug!("wake task {} (sepc {:#x})", t.id, t.resume_sepc);
}

/// 仅含一条 `wfi` 的辅助函数：wfi 是 hint，可能阻塞至中断，也可能因
/// 中断已挂起而直接返回。
#[inline(never)]
fn sleep_wfi() {
    // SAFETY: wfi 不触碰内存/栈。
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }
}

/// 唤醒恢复点：仅含一条 `ret`。
///
/// wake_task 把 Blocked 任务的 sepc 重置到此处；`ret` 借助保存的 `ra` 跳回
/// sleep() 调用方。若 trap 落在 wfi（`ra` = sleep 内调用点之后），经 sleep
/// 的 epilogue 正常返回；若落在 `csrs SIE` 与 wfi 之间的窗口（`ra` 尚未被
/// 内部调用修改），则直接跳回调用方——两条路径都正确继续。
#[inline(never)]
fn resume_after_sleep() {}

/// 阻塞当前任务一段时长（[`Duration`]，按 timebase 频率换算为 mtime 刻度）。
///
/// 换算公式：`ticks = secs × freq + subsec_nanos × freq / 1e9`（saturating，
/// 超大时长不 panic，只睡到 u64 能表达的极限）。例如 `sleep(Duration::from_secs(1))`
/// 在 10 MHz timebase 上阻塞 10_000_000 个 mtime 刻度，即 1 秒。
///
/// 关全局中断置 Blocked 与唤醒时刻，再开中断进入 `wfi`。无论被抢占点落在
/// 何处（wfi 阻塞、wfi 因挂起中断立即返回、或 `csrs`/`wfi` 之间），唤醒时
/// wake_task 都把 sepc 重置到 [`resume_after_sleep`]，本函数正常返回。
///
/// `wfi` 是 hint：中断已挂起时可能直接返回（任务并未被 park）。此时恢复段
/// 会把 state 从 Blocked 复位为 Ready——否则任务带着 Blocked 状态继续运行，
/// 下次抢占会被误 park 进睡眠列表（见 `wfi` 之后的恢复代码）。
///
/// 约束：不持有任何锁时调用（SIE=0 时 wfi 永不醒）。
pub fn sleep(d: Duration) {
    let freq = crate::platform::get().timebase_frequency;
    // Duration → mtime 刻度：整秒部分 × 频率，亚秒部分按比例换算。
    let deadline = now_ticks()
        .saturating_add(d.as_secs().saturating_mul(freq))
        .saturating_add(
            (d.subsec_nanos() as u64)
                .saturating_mul(freq)
                .saturating_div(1_000_000_000),
        );

    // SAFETY: S-mode 下允许开关全局中断。
    unsafe {
        crate::hal::csr::sstatus::clear(crate::hal::csr::sstatus::Sstatus::SIE);
    }
    {
        // TrapGuard 看到 SIE 已关，drop 后不恢复，整个区域中断关闭；
        // 保证第一个可能落地中断看到的已是 Blocked 状态。
        let mut cur = CURRENT.lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Blocked;
            t.wake_tick = deadline;
            t.resume_sepc = resume_after_sleep as *const () as usize;
        }
    }
    // 开中断后进入 wfi。clear/set 两条 asm 均为编译器屏障，
    // 状态置位不会被移出临界区。
    // SAFETY: 只写 sstatus.SIE 位。
    unsafe {
        crate::hal::csr::sstatus::set(crate::hal::csr::sstatus::Sstatus::SIE);
    }
    sleep_wfi();
    // 走到此处 = 任务恢复运行，两种路径：
    //   ① 睡眠到期唤醒：wake_task 已把 state 置 Ready（sepc 重置到
    //      resume_after_sleep，ret 跳回此处）；
    //   ② wfi 直接返回（hint：中断已挂起时不阻塞）——state 仍是 Blocked、
    //      wake_tick 在未来，若不加处理，下次抢占会被误 park 进睡眠列表。
    // 统一恢复 Ready 对两条路径都正确。关中断缩小"恢复前被抢占"的窗口：
    // 若此刻再被抢占，scheduler 看到的已是 Ready，走普通重排。
    // SAFETY: S-mode 下允许开关全局中断。
    unsafe {
        crate::hal::csr::sstatus::clear(crate::hal::csr::sstatus::Sstatus::SIE);
    }
    {
        let mut cur = CURRENT.lock();
        if let Some(t) = cur.as_mut() {
            if t.state == TaskState::Blocked {
                t.state = TaskState::Ready;
            }
        }
    }
    // SAFETY: 只写 sstatus.SIE 位。
    unsafe {
        crate::hal::csr::sstatus::set(crate::hal::csr::sstatus::Sstatus::SIE);
    }
}
