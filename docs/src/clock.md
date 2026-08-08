# clock 模块 API 契约

> 本文件是 `src/clock/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Module structure clock 树」迁出。

## 1. 职责

时钟子系统——**唯一时间入口**（依赖单向 `scheduler → clock → hal`）。提供时间源、换算、tick、软定时器、忙等延时。

边界：

- **不碰调度**：`tick.rs` 消费 hal 能力，不碰调度；调度器经 clock 读时间。
- **不与 `schedule::sleep` 混淆**：忙等延时无任务上下文可用，语义区分（见 `schedule.md`）。

## 2. 引导流程

```
init Phase 3: clock::tick::start()   ← 首次装载 10ms 定时中断（原 CLINT probe 装载已上移至此）
之后 STI 由 tick::on_timer 重装 + jiffies++ + 软定时器分发
```

## 3. 公共 API 面

### 3.1 文件分布

| 文件 | 内容 |
|------|------|
| `source.rs` | 时钟源：`Clock` 契约 + `CsrClock` + `init/now/frequency`（未注册 panic） |
| `convert.rs` | 时间换算唯一处：`Duration↔ticks↔usec`（饱和防溢出） |
| `tick.rs` | `TICK_HZ=100`（10ms）+ `jiffies` + `tick::start`/`on_timer` |
| `timer.rs` | 软定时器队列：一次性/周期回调，tick 分发（回调中断上下文、锁外执行） |
| `delay.rs` | 忙等延时（无任务上下文可用） |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 时间换算唯一处 | 跨模块统一换算集中在 `convert.rs`，不在各自实现 |

## 5. 生命周期与并发

- 依赖单向：scheduler → clock → hal；不反向。
- 定时器回调在中断上下文、锁外执行。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（clock 树）。
