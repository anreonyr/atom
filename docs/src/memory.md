# memory 模块 API 契约

> 本文件是 `src/memory/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Module structure memory 树 + Key patterns（allocator/MMU）」迁出。

## 1. 职责

内存管理：Portal 分配器体系 + Sv39 页表 + 地址空间 + 缺页处理。Sv39 identity-mapped（内核低半区）。

边界：

- **不做任务栈/守护页**：任务栈布局与守护页在 `schedule`/`runtime` 侧。
- **不做用户堆状态**：envcall map/unmap 走本模块匿名页分配（由 `schedule`/`runtime` 侧组合）。

## 2. 引导流程

```
allocator::init()   (bump → hybrid)
  → space::init()   (Sv39 根页表 + KERNEL_SPACE)
```

driver probe 期间经 `memory::map_device()` 做 MMIO identity 映射（每驱动 probe 内完成）。

## 3. 公共 API 面

### 3.1 文件分布

| 路径 | 内容 |
|------|------|
| `allocator/portal` | `#[global_allocator]` — delegates to `&dyn Allocator` |
| `allocator/bump` | Early boot bump allocator (before heap init) |
| `allocator/hybrid` | Buddy + Segregated Free List 组合分配器 |
| `allocator/block` | Block allocator (segregated free list) |
| `allocator/frame` | Buddy physical page allocator |
| `allocator/page` | MMU page-frame allocator (frame → page) |
| `addr.rs` | `VirtAddr` / `PhysAddr` / `PhysPage` |
| `entry.rs` | Sv39 PTE + `PteFlags` |
| `fault.rs` | Page fault capture + handling |
| `space.rs` | `AddressSpace` (root page table + map/walk/unmap) |
| `asid.rs` | 16 位 ASID 分配器（0 保留内核；from_kernel 每空间独立分配 / Drop 释放，释放时 sfence 清该 ASID 残留） |
| `table.rs` | Sv39 three-level page table |

### 3.2 关键模式

- **Allocator**：`portal` 持有 `&dyn Allocator` trait object，启动时切换：`None` → `bump` (early) → `hybrid` (runtime)。
- **MMU**：Sv39 identity-mapped；`KERNEL_SPACE` 用 `RelLock`（可重入——缺页 handler 重入同 hart）。

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 不用缩写 | `VirtAddr`/`PhysAddr`/`PhysPage` 全称 |
| 查询 API 用查找动词 | `find`/`get` 表达查找，返回 `Option` |

## 5. 生命周期与并发

- allocator 切换有严格时序：bump 仅在 heap 初始化前可用，之后 portal 指向 hybrid。
- `KERNEL_SPACE` 用 `RelLock`（可重入，缺页 handler 重入同 hart 合法）。
- ASID 分配/释放成对：`from_kernel` 每空间独立分配，`Drop` 释放时 sfence 清该 ASID 残留。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（memory 树 + allocator/MMU 关键模式）。
