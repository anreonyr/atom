# sbi 模块 API 契约

> 本文件是 `src/sbi/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「SBI interface」迁出。

## 1. 职责

SBI ecall 封装（独立原子目录）：把 M-mode 服务经 ecall 委托给 OpenSBI。仅三件：
`set_timer` / `system_reset` / `putchar`。

边界：

- **不碰 CSR**：定时器读 `time` CSR（Sstc，S-mode 可访问），不是 CLINT MMIO（PMP-blocked）。
- **不承担平台探测**：`timebase_frequency` 等在 `platform/`（见 `platform.md`）。

## 2. 引导流程

无引导时序；`putchar` 在 panic 路径也需可用（panic-safe raw output）。

## 3. 公共 API 面

### 3.1 调用约定

`ecall(a7=EID, a6=FID, a0..a5)`

### 3.2 扩展表

| Extension | EID | Functions |
|-----------|-----|-----------|
| Legacy | `0x01` (console) | `putchar(ch)` — panic-safe raw output |
| TIME | `0x54494D45` | `set_timer(absolute_time)` — delegates mtimecmp to M-mode |
| SRST | `0x53525354` | `system_reset(type, reason)` — shutdown/reboot |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 错误码语义即行为 | 各函数按 SBI 返回码语义处理 |

## 5. 生命周期与并发

- panic 路径可用：SBI ecall 不依赖内核锁/堆。
- S-mode 自身 ecall（scause=9）不经 trap_handler：MEDELEG bit9=0，留 M-mode OpenSBI。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（SBI interface）。
