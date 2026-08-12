# schedule 模块 API 契约

> 本文件是 `src/schedule/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 设计意图见 `../task-model.md`；本文档为实现契约。
> 从 CLAUDE.md「Module structure schedule 树」迁出。

## 1. 职责

round-robin 调度器：task / scheduler / sleep / spawn / exit / wait / kill / yield。
`TaskTable` 单锁门面聚合就绪/睡眠/僵尸队列 + `CURRENT`，`Box<Task>` 稳定句柄。

边界：

- **时间读取走 clock**（依赖单向 scheduler → clock → hal）。
- **不做忙等延时**：忙等延时在 `clock::delay`（无任务上下文可用），与 `schedule::sleep` 语义区分。

## 2. 引导流程

```
main: schedule::spawn(Entry::Kernel(task_a), None) → WFI idle loop
  之后 STI → clock::tick::on_timer() → scheduler::scheduler() 驱动轮转
```

`spawn(entry: Entry, space: Option<Box<AddressSpace>>)` 是唯一任务创建入口（`Entry` 定义于此；
`Entry::Kernel`/`Entry::User` 语义见 `runtime.md`）。

## 3. 公共 API 面

### 3.1 文件分布

| 文件 | 内容 |
|------|------|
| `task.rs` | `Task` 结构 + 状态（就绪/睡眠/僵尸）；`Pending::Event(Event)` + `Event` enum（Input/Block，类型化事件标识） |
| `scheduler.rs` | 轮转调度 + `CURRENT` |
| `spawn.rs` | `spawn` 统一入口，返回任务 id；`Entry::Kernel`/`Entry::User` |
| `sleep.rs` | 时间阻塞（`park_current` 置阻塞意图 + self-IPI 立即 park）+ **统一事件等待/通知原语**（`mark_event_wait`/`signal_event`/`wait_event`/`interrupts_enabled`） |
| `wait.rs` | 事件阻塞 + 僵尸延迟回收 + 退出码；`wait_sys`（syscall 版，trap 上下文 re-dispatch） |
| `exit.rs` | 任务退出（标 Reap） |
| `kill.rs` | 他杀 |
| `yield.rs` | `r#yield` self-IPI 立即重排 |

### 3.2 语义

- spawn 统一入口返回任务 id；sleep 时间阻塞 / wait 事件阻塞 + 僵尸延迟回收 + 退出码；kill 他杀。
- 僵尸语义：父存活但从不 wait → 悬挂保留（任务数有限，与 Linux 一致）；孤儿/已收尸立即回收。

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 查询 API 用查找动词 | `spawn -> id`；查找类返回 `Option` |
| 读写对称 | 状态字段读 = 字段名，写 = `set_` |

## 5. 生命周期与并发

- `TaskTable` 单锁门面：就绪/睡眠/僵尸队列 + `CURRENT` 聚合，一次获取。
- `Box<Task>` 稳定句柄：任务对象地址在堆上稳定，不被移动。

## 6. 变更记录

- 2026-08-10：阻塞语义确定性——`sleep`/`wait`/`exit` 改经 `park_current`（关中断下置阻塞意图 + self-IPI 保证在 `park_wfi` 叶函数处 park，唤醒后沿 epilogue 链回卷）；删除 `sleep` 的「wfi 未 park 复位」恢复段（修复 `sleep` 可能提前返回、睡不满的缺陷）。`input_wait`/`wait_event` 保留 wfi 提示语义（事件阻塞的到达窗口由「wfi 返回 + 调用方重读」兜底）。`scheduler`/`trap`/`yield` 零改动。
- 2026-08-08：M5——`wait_sys`/`WaitSys`（syscall 版等待，M5 spawn/wait/kill syscall 用）：
  trap 上下文置 `Pending::Wait(pid)` + resume_sepc=0 → `Reschedule` park，唤醒后 sret 重放
  ecall 重入读 `wait_result`（与 `schedule::wait` 的就地 wfi 阻塞并存）；scheduler Reap 与
  kill 的唤醒恢复点按 `TaskKind` 区分（UMode→0 重放 / SMode→`resume_after_wait` 原地）。
- 2026-08-08：M4——统一事件等待原语：`Pending::WaitRead` → `Pending::Event(Event)`（类型化 enum
  `Event::Input`/`Event::Block`），`mark_input_wait`/`wake_input_waiters` 泛化为
  `mark_event_wait(id, resume)`/`signal_event(id)` + `wait_event(id, done)`（SIE=1 wfi 中断唤醒、
  SIE=0 轮询兜底）；console 输入与块完成共用同一原语（未来 IPC 基础）。`sleep`/`wait(pid)`/`kill`
  语义不变。
- 2026-08-08：从 CLAUDE.md 迁出（schedule 树）。
