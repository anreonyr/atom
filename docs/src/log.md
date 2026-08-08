# log 模块 API 契约

> 本文件是 `src/log/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Module structure log 树」迁出。

## 1. 职责

日志格式化层：`error!`/`warn!`/`info!`/`debug!`/`trace!`（输出经 `file::io::print` 的 `println!`）。

边界：

- **不做直接设备输出**：输出目标走 `file::io::print`（见 `file.md`）。
- **`/dev/log` 暂缺**：因 log → file::io 输出依赖成环而移除（`log_read` 预留，未来经 `file::registry` 恢复）。

## 2. 引导流程

```
init Phase 2: log::init_timestamp() → log::set_max_level()
```

## 3. 公共 API 面

### 3.1 文件分布

| 文件 | 内容 |
|------|------|
| `mod.rs` | 输出编排：`_log`/`_log_force`/`log_line` + 对外 API 重新导出 |
| `filter.rs` | 级别（`LogLevel`）+ 三层过滤 + console 级别（console_loglevel 对应物） |
| `record.rs` | `LogMessage`/`Timestamp`（owned 定长消息结构，console 与 ring 同一结构） |
| `palette.rs` | ANSI 调色板（`Color` 唯一转义来源 + `LEVEL_META` 级别样式） |
| `buf.rs` | 定长栈缓冲（无堆 `fmt::Write` 目标） |
| `ring.rs` | 最近日志环形快照（`LogMessage`/`LogRing`/`log_read`/`log_seq_range` → 预留 /dev/log） |
| `macros.rs` | 日志宏（`log!`/`error!`/`warn!`/`info!`/`debug!`/`trace!`） |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 语义即行为 | 五级命名 `error/warn/info/debug/trace` 与 Linux 日志级别对应 |

## 5. 生命周期与并发

- 输出经 `file::io::print` 的 `println!`（`OUT` 锁仲裁）。
- `log_read` 预留：未来经 `file::registry` 恢复 /dev/log（需先解决环依赖）。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（log 树）。
