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
// 按职责拆分为五个文件：
//   task.rs     任务数据结构（Task/TaskKind/TaskState/Pending）与全局调度状态
//               （TASK_QUEUE/SLEEP_LIST/ZOMBIE_LIST/CURRENT）及查询
//   schedule.rs 调度核心：scheduler() 主循环 + zombie 回收 + 到期唤醒
//   sleep.rs    阻塞-唤醒状态机：sleep/wake_task、mtime 换算、跨任务物理帧访问
//   spawn.rs    任务创建：spawn/spawn_with（栈帧 + 地址空间 + 初始 TrapFrame）
//   exit.rs     任务退出：exit/terminate_current
// 对外 API 在本文件统一重导出，调用方 `crate::scheduler::X` 路径保持不变。

mod exit;
mod schedule;
mod sleep;
mod spawn;
mod task;

pub use exit::exit;
pub(crate) use exit::terminate_current;
pub use schedule::scheduler;
pub use sleep::sleep;
pub use spawn::{spawn, spawn_with};
pub(crate) use task::current_is_umode;
pub use task::current_space;
