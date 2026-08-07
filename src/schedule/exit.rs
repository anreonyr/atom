// 任务退出（scheduler 子模块）
//
// exit() 是公共 kill API 的唯一入口：标 Zombie 后 wfi 等 tick 来 park（回收
// 在下一个 scheduler() 开头）；terminate_current 是 trap 侧入口——trap 态
// 持有刚保存的 TrapFrame，可直接 dispatch 下一任务并返回其帧。

use crate::context::TrapFrame;
use crate::info;

use super::{
    scheduler::scheduler,
    task::{Pending, TASK_TABLE, current_id},
};

/// 任务态退出（公共 kill API 的唯一入口）：标 Zombie 后 wfi 等 tick 来 park。
///
/// sepc 无关（僵尸永不恢复），无需原子序列。约束：不持有任何锁时调用
/// （SIE=0 时 wfi 永不醒）；不得由 boot/空闲任务调用。
pub fn exit(code: i32) -> ! {
    info!("task {} exit(code={})", current_id(), code);
    {
        let mut table = TASK_TABLE.lock();
        if let Some(t) = table.current_mut() {
            t.exit_code = Some(code);
            t.pending = Pending::Reap;
        }
    }
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// trap 态终止当前任务（trap.rs 未处理用户缺页、envcall exit 调用）。
///
/// 与 [`exit`] 是同一个"杀当前任务"语义的 trap 侧入口：trap 态持有刚保存的
/// TrapFrame，可立即 dispatch 下一任务并返回其帧。退出码由调用方指定：
/// 缺页终止传 -1（等效 SIGSEGV，wait 收尸时读到）；envcall exit(code) 传
/// 用户退出码。属跨模块内部辅助。
pub(crate) fn terminate_current(frame: *mut TrapFrame, code: i32) -> usize {
    {
        let mut table = TASK_TABLE.lock();
        if let Some(t) = table.current_mut() {
            t.exit_code = Some(code);
            t.pending = Pending::Reap;
        }
    }
    scheduler(frame)
}
