// 主动让出 CPU（scheduler 子模块）
//
// r#yield() 置 sip.SSIP 触发 self-IPI：下一中断（SSI）到来时 trap_vector 清
// 标志并调用 scheduler(frame)，当前任务按 Pending::Rerun 重排到就绪队列
// 队尾（round-robin 立即让出），不等 10ms tick。依赖 sie.SSIE 已使能
// （main.rs 启动序列：sie::set(Sie::SSIE)）。
//
// 函数名 `yield` 是保留关键字，定义/调用处必须写 raw identifier `r#yield`
// （`r#` 强制按标识符解析，真实名字就是 `yield`）。

/// 主动让出 CPU：当前任务重排到就绪队列队尾（round-robin 立即让出），
/// 与抢占式 tick 的区别是**立即**生效而非等下一个 10ms。
///
/// 实现：置 sip.SSIP（S-mode 软件中断挂起位）触发 self-IPI；trap 侧清标志
/// 后调用 scheduler()——若当前任务无待办（pending=Rerun）则重排到队尾；
/// 若刚 sleep/exit/wait（pending=Park/Reap/Wait）则按各自处置迁移。
///
/// 约束：须在中断使能后调用（SIE=1 且 sie.SSIE=1，main.rs 启动序列保证）；
/// 中断关闭时置位将挂起至开中断，语义不变。
pub fn r#yield() {
    // SAFETY: 单 hart，置位无副作用；SSI 由 trap_vector 清标志（trap.rs）。
    unsafe { crate::hal::csr::sip::set(crate::hal::csr::sip::Sip::SSIP) };
}
