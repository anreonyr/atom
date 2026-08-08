// 任务调度器 — trap 驱动的 round-robin 抢占 + 就绪/阻塞/僵尸状态机
//
// 时钟中断 → trap_vector 把上下文保存到任务栈顶的 TrapFrame → scheduler(frame)：
// 处置当前任务（Running 按 Pending 迁移：Rerun 重排 / Park 入睡眠列表 / Reap 入
// 僵尸列表），弹出下一就绪任务，切换地址空间，返回其 TrapFrame 供 trap_vector 恢复。
//
// 每个任务运行在独立的地址空间（spawn 自动建 `from_kernel` 克隆），栈映射到
// 固定虚拟窗口（任务栈常量见 crate::memory 模块），窗口下方一页留空作守护页
// ——栈溢出直接触发缺页（user → terminate / kernel → panic）。
// `current_space()` 从 CURRENT 推导，缺页处理器据此路由。
//
// 按职责拆分为八个文件：
//   task.rs     任务数据结构（Task/TaskKind/TaskState/Pending/WaitResult）与
//               调度状态门面 TaskTable（就绪/睡眠/僵尸队列 + CURRENT，单锁）
//               及查询（current_space/current_id/current_is_umode）
//   scheduler.rs 调度核心：scheduler() 主循环 + 僵尸延迟回收 + 到期唤醒
//   sleep.rs    时间阻塞：sleep/wake_task、mtime 换算、跨任务物理帧访问
//   spawn.rs    任务创建：spawn + Entry（栈帧 + 地址空间 + 初始 TrapFrame）
//   exit.rs     任务退出：exit/terminate_current（自杀域 + 退出码）
//   wait.rs     父子回收 + 事件阻塞：wait（收尸 / 阻塞等子退出）
//   kill.rs     他杀域：kill（终止目标任务 + 唤醒等待者）
//   yield.rs    主动让出：r#yield（self-IPI 立即重排）
// 对外 API 在本文件统一重导出，调用方 `crate::schedule::X` 路径。

mod exit;
mod kill;
mod scheduler;
mod sleep;
mod spawn;
mod task;
mod wait;
mod r#yield;

pub use exit::exit;
pub(crate) use exit::mark_reap;
pub(crate) use exit::terminate_current;
pub use kill::{KillError, kill};
pub use scheduler::scheduler;
pub use sleep::sleep;
// wait_event/interrupts_enabled 在阶段 B 块驱动消费时再导出（当前无调用点）
pub(crate) use sleep::{clear_event_wait, input_wait, mark_event_wait, signal_event};
pub use spawn::{Entry, TaskBuilder};
pub(crate) use task::current_id;
pub(crate) use task::Event;
pub(crate) use task::current_is_umode;
pub use task::current_space;
pub use wait::wait;
pub use r#yield::r#yield;
