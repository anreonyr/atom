# atom 任务模型

> 本笔记沉淀任务/线程/进程模型的**设计决策**——为什么 atom 没有"进程"概念、
> 为什么不需要单独实现 fork/exec、以及未来如何在现有模型上扩展。
> 代码事实以 `src/schedule/`、`src/memory/space.rs` 为准。

## 1. 统一任务模型

atom 只有**一个调度单元 `Task`**,没有 Linux 的 task_struct / mm_struct 双类型。
地址空间关系由构造方式参数化:

```
线程 = 通过 Arc 共享同一 AddressSpace 的多个 Task
任务 = 独占 AddressSpace 的单个 Task(Arc 计数恒 1)
```

`TaskBuilder` 两个入口即两个端点:

```rust
TaskBuilder::new(entry).space(space).spawn()      // 独占:任务
TaskBuilder::new(entry).shared(arc).spawn()       // 共享:线程
```

两个正交的"参数化"维度:

- **`SpaceRef`**(`spawn.rs`)——空间归属:`Owned(Option<Box<AddressSpace>>)` /
  `Shared(Arc<AddressSpace>)`
- **`Entry`**(`spawn.rs`)——执行入口:`Kernel(fn())`(S-mode)/ `User(VirtAddr)`(真 U-mode)

Linux 需要 fork/exec/clone 三个原语,是因为它先有进程、再纠结共享方式;
atom 没有进程概念,只有"任务 + 空间归属",fork/exec 要解决的问题分别落在
`SpaceRef` 与 `Entry` 参数上——**状态机复杂度被参数化吸收**。

## 2. 地址空间三种共享形态

| 形态 | 机制 | 共享对象 | 用途 |
|---|---|---|---|
| **浅克隆** | `from_kernel(alloc)` | 内核半区 L2 子树(DRAM identity/MMIO/高半区,`shared_l2` 记录) | 任务:独立用户区 + 复用内核映射 |
| **实例共享** | `Arc::clone(&space)` | 整个 `AddressSpace`(页表树/ASID/用户堆/Region 表) | 线程:同一空间多执行流 |
| **半区复制** | `share_kernel(&self, kernel)` | 仅内核半区 PTE(256-511 拷贝) | 预留(fork 方向);当前无调用点 |

`from_kernel` 与 `Arc` 覆盖了"最小成本独立"与"零成本共享"两个极端;
`share_kernel` 落在中间(复制内核、独立用户),当前只有 fork 类需求才用得上,
故保持 `#[allow(dead_code)]` 预留。

## 3. 线程语义

**共享的东西**(同一 `Arc<AddressSpace>`):
- 页表树(用户 + 内核映射)——共享内存即通信
- ASID(同一空间,TLB 同一标签)
- 用户堆(`heap_next`/`heap_blocks`——`map`/`unmap` syscall 线程间可见)
- Region 表(Anonymous 缺页区域)

**独立的东西**(每个 `Task` 自己):
- 栈(动态窗口:`stack_window_va(slot)` 单调错开,窗口 0 = `TASK_STACK_BASE`)
- 执行上下文(`TrapFrame`、sepc、寄存器)
- 调度状态(priority、ticks_left、pending、state)
- 任务身份(id——`wait`/`kill` 的句柄)

**生命周期 = Arc 引用计数**:
- 线程退出 → 只回收自己的栈 + unmap 自己的栈窗口(`reclaim` 对
  `Arc::strong_count > 1` 先 unmap),空间引用减一
- 空间释放 = 最后一个持有它的任务退出;组长先退不影响线程存活

**同步**:单 hart + 关中断天然串行;调度器内 `SpinLock` 串行化队列访问。

## 4. fork/exec 取舍

| Linux 原语 | atom 的对应 | 状态 |
|---|---|---|
| `fork`(复制进程) | 被线程(`shared`)与 `spawn`(从零建空间)分而治之 | 不单独实现 |
| `exec`(替换镜像) | `spawn(Entry::User, space)` = "新空间 + 入口";loader 只是生产 `(space, entry)` 的工厂 | 不单独实现 |
| `clone`(共享参数化) | `SpaceRef` 枚举直接表达 | 已吸收 |

**为什么不需要 fork**:它解决的"复制进程"问题,教学场景没有状态复制诉求;
线程 + 从零建空间覆盖 99% 需求。**为什么 exec 不需要独立机制**:
`spawn` 已承载"新空间 + U 入口"骨架,`map_user_code`(内嵌汇编装载)就是
最简 loader 的雏形。

**代价(诚实)**:牺牲了进程复制与任务自换镜像;`share_kernel` 因此闲置。
这两者都**能在现有模型上补**(见下节),不是设计死路。

> M5(2026-08-08)定夺:**不实现 fork/exec 语义**。程序需求链终点只需「派生子进程跑
> 另一个程序 + 回收退出码」——由 `spawn(blob)` syscall 满足(loader 装载进新空间,见
> §5 落地注)。fork(复制进程)、exec(自换镜像)维持本表判断,不单独实现;若未来教学
> 需要,`clone_user`/`exec_current` 仍是消费既有原语的可选增量。

## 5. 未来扩展落点(全部消费既有原语)

| 功能 | 落点 | 需要新增 |
|---|---|---|
| ELF loader | `TaskBuilder::elf(blob)`(方案 A 静态工厂,不动 `Entry`)或 `Entry::Elf` 变体 | `loader/` 模块:ELF 解析 + 段装载(代码 R\|X、数据 R\|W、BSS 零页)→ `(space, entry_va)` |
| 任务自换镜像(类 exec) | `TaskTable::exec_current(new_space, entry)`:换 `space` 字段 + 改 `frame.sepc`;`Arc::strong_count == 1` 才可换(被共享则拒绝) | 一个 TaskTable 方法 |
| 进程复制(类 fork) | `AddressSpace::clone_user(&self)`:逐页 `translate` 读 PA → 新空间 `map` + `share_kernel` 继承内核半区;初始帧 = 父帧副本(sepc 保持) | 一个复制函数 + spawn 变体 |

依赖顺序:`loader` 是地基(后两者都依赖它),`exec_current`/`clone_user` 是
可选增量。`share_kernel` 的消费点正是 `clone_user`。

> 已落地(2026-08-08,M1):ELF loader = `TaskBuilder::loader(blob)` 静态工厂
> (方案 A 形态,不动 `Entry`),`loader/` 模块产 `(space, entry_va)`,`user/` 子
> crate 提供无 libc 静态 ELF 探针。见 [[loader]] 契约文档。
>
> 已落地(2026-08-08,M5):`spawn(blob)` syscall——U 程序从自身内存拷出 ELF → loader
> 装载进新空间 → 子任务跑新程序,父 `wait` 回收退出码。fork/exec 未实现(§4 定夺)。

## 6. 关键设计约束

- `SpaceRef` 是三态演进空间:将来若做 COW(写时复制)就是加第三态
  (`Shared` + 页级写保护)——Arc 计数既服务线程共享、也服务未来 COW 判断。
- `spawn(Entry::User)` 本质是"线程与 fork+exec 的混合体":创建新任务 + 新空间
  (从零开始,不复制父用户区),即 exec 语义的新空间版。
- 栈窗口/堆状态都挂在 `AddressSpace` 上 → 线程共享空间天然共享堆与窗口分配;
  独立任务的这些状态随空间回收。
- 相关:[[api-naming-self-consistency]](新 API 先列面,命名入框架)、
  [[api-stability-preference]](既有 API 如 `Entry`/`spawn` 签名不动,新增优先)。

## 7. 阻塞与恢复机制（park_current / self-IPI）

基于时间/处置的阻塞（`sleep`/`wait`/`exit`）经 `park_current`（`schedule/sleep.rs`）
统一实现，与旧「置 pending + wfi 等 tick」的关键差异：

- **确定性 park**：关中断下原子置阻塞意图（`pending` + `wake_tick`/`resume_sepc`/
  `wait_pid`/`exit_code`），随后置 SSIP 触发 self-IPI，`park_wfi`（叶函数）开中断
  后 SSI 恰好落其 wfi 指令 → trap 侧清 SSIP + `scheduler()` 立即按 `pending` 迁移。
  任务**必然被 park**，不再依赖 wfi 提示语义——修复了旧 `sleep()` 可能提前返回
  （睡不满）的缺陷。
- **恢复点安全（关键约束）**：`park_wfi` 必须保持叶函数形态（仅 `csrs SIE` + `wfi`，
  无序言、无调用）。trap 保存帧的 ra/sp 必须与调用方帧一致（帧内一致），唤醒后
  `wake_task` 把 sepc 重置到恢复点（`resume_after_sleep`/`resume_after_wait`，均仅
  含 `ret`），沿「park_wfi → park_current → 入口」的 epilogue 链逐级回卷。
  SIE 从「置意图」到 `park_wfi` 全程关闭——任何 SIE=1 窗口都会让 tick 在中间帧
  处 park 任务，破坏回卷链。
- **事件阻塞保留 wfi 提示语义**：`input_wait`/`wait_event`（输入/块完成）不用
  self-IPI。事件在 wfi 窗口到达时，wfi 立即返回 + 恢复段复位 + 调用方重读是
  事件到达窗口的兜底机制；若强制 park，事件在 park 窗口到达会被 `signal_event`
  漏掉（扫描睡眠队列时任务尚未入队）而永久挂死。
- `wait(pid)` 的 park 处置（scheduler `Pending::Wait` 分支）先查僵尸队列——子先退
  时当场收尸唤醒，闭合「子退在父 park 前」的窗口；子存活才入睡眠队列等 Reap/kill。

`park_current` 与 `pending` 的解耦（state/pending 双字段）保持不变：任务代码只改
`pending`，真实迁移始终收敛在 `scheduler()`。
