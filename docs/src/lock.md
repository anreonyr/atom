# lock 模块 API 契约

> 本文件是 `src/lock/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Lock choices」迁出。

## 1. 职责

锁 / 同步原语家族（原子）：`SpinLock` / `BareLock` / `RwLock` / `RelLock` / `OnceLock` / `LazyLock`。

边界：

- **不做任务调度**：阻塞语义由 `schedule::sleep`/`wait` 提供，锁只是临界区原语。

## 2. 引导流程

无引导时序；allocator 前的启动阶段可用 `BareLock`/`OnceLock`（无需关中断/无堆）。

## 3. 公共 API 面

### 3.1 锁选型

| Lock | Interrupt-safe | Use case |
|------|---------------|----------|
| `SpinLock` | Yes (closes SIE) | Default, interrupt contexts；lockdep：入口捕获调用者返回地址（`holder_pc`），单 hart 下递归获取在死循环前报告 + panic |
| `BareLock` | No (unsafe fn) | Task-only, no interrupt masking；lockdep 同 SpinLock（重入检测 + `holder_pc`） |
| `RwLock` | Yes | Read-heavy data (hub devices table)；lockdep：写重入 / 读→写升级 / 写→读降级三死锁形态检测 + 写持有者 `holder_pc`（读重入合法不检测） |
| `RelLock` | Yes (reentrant) | Page fault handler (re-enters same hart)；重入合法，仅 `holder_pc` 最外层调用点溯源 |
| `OnceLock` | Read-path safe | Write-once, read-many (log mtime, etc.) |

（`LazyLock` 同 OnceLock 家族，懒初始化。）

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 不用缩写 | 锁类型名全称拼写（`SpinLock`/`BareLock`/`RwLock`/`RelLock`/`OnceLock`/`LazyLock`） |

## 5. 生命周期与并发

- Interrupt-safe 锁关中断（SpinLock/RwLock/RelLock）；`BareLock` 由调用方保证只在任务上下文用。
- lockdep：入口捕获调用者返回地址（`holder_pc`）做重入/死锁形态检测，单 hart 下死锁前报告 + panic。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（Lock choices）。
