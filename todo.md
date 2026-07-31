# 分配器重写 — 影响范围梳理

<!--toc:start-->
- [分配器重写 — 影响范围梳理](#分配器重写-影响范围梳理)
  - [分配器模块结构](#分配器模块结构)
  - [1. 全局分配器入口](#1-全局分配器入口)
    - [1.1 `#[global_allocator]` 注册](#11-globalallocator-注册)
    - [1.2 `HybridAllocator` — GlobalAlloc 实现](#12-hybridallocator-globalalloc-实现)
  - [2. 物理帧分配器 (`frame`) 的使用者](#2-物理帧分配器-frame-的使用者)
    - [2.1 MMU 初始化 — 创建内核地址空间](#21-mmu-初始化-创建内核地址空间)
    - [2.2 AddressSpace — 页表生命周期](#22-addressspace-页表生命周期)
    - [2.3 PageTable — 底层帧分配/释放](#23-pagetable-底层帧分配释放)
  - [3. 全局堆分配器 (`#[global_allocator]`) 的间接触发者](#3-全局堆分配器-globalallocator-的间接触发者)
    - [3.1 调度器 — 任务栈分配](#31-调度器-任务栈分配)
    - [3.2 陷阱处理 — 外部中断注册表](#32-陷阱处理-外部中断注册表)
    - [3.3 物理帧分配器 — 位图存储](#33-物理帧分配器-位图存储)
    - [3.4 日志 / 格式化](#34-日志-格式化)
  - [4. 初始化调用链](#4-初始化调用链)
    - [4.1 启动序列](#41-启动序列)
    - [4.2 关键依赖关系](#42-关键依赖关系)
  - [5. `framealloc` 新模块](#5-framealloc-新模块)
  - [6. 重写时需要关注的文件清单](#6-重写时需要关注的文件清单)
    - [必须修改](#必须修改)
    - [可能需要修改（取决于接口兼容性）](#可能需要修改取决于接口兼容性)
    - [不需要修改（仅间接依赖全局分配器）](#不需要修改仅间接依赖全局分配器)
  - [7. `alloc` crate 类型使用汇总](#7-alloc-crate-类型使用汇总)
<!--toc:end-->

## 分配器模块结构

当前分配器位于 `src/allocator/`，包含 5 个子模块：

| 模块 | 文件 | 职责 |
| ------ | ------ | ------ |
| `hybrid` | `src/allocator/hybrid.rs` | `#[global_allocator]` 入口，在 Buddy 和 Slab 间分发 |
| `buddy` | `src/allocator/buddy.rs` | Buddy 幂次分配器（64 KiB 堆，`.bss` 节） |
| `slab` | `src/allocator/slab.rs` | Slab 对象缓存（≤512B, align≤8, 独立页池 `.bss`） |
| `page` | `src/allocator/page.rs` | 位图物理页分配器（6 MiB DRAM） |
| `framealloc` | `src/allocator/framealloc.rs` | **新模块**（目前仅骨架，尚未接入） |
| `mod` | `src/allocator/mod.rs` | `init()` 入口，调用 `buddy::init()` + `frame::init()` |

---

## 1. 全局分配器入口

### 1.1 `#[global_allocator]` 注册

- **`src/allocator/hybrid.rs:64`** — `static ALLOCATOR: HybridAllocator`
- **`src/main.rs:3`** — `#![feature(allocator_api)]`（unstable feature gate）
- **`src/main.rs:4`** — `extern crate alloc;`（引入 `alloc` crate）
- **`src/main.rs:6`** — `mod allocator;`

> **重写注意**：如果替换 `#[global_allocator]`，需同步更新/移除 `feature(allocator_api)`。

### 1.2 `HybridAllocator` — GlobalAlloc 实现

- **`src/allocator/hybrid.rs:24-61`** — `unsafe impl GlobalAlloc`
  - `alloc()`: size≤512 && align≤8 → slab 快速路径 → 回退 buddy
  - `dealloc()`: 同样按 size/align 分流到 slab 或 buddy

> **重写注意**：新 `#[global_allocator]` 必须提供等价的 `alloc`/`dealloc` 实现。

---

## 2. 物理帧分配器 (`frame`) 的使用者

物理帧分配器通过 `core::alloc::Allocator` trait 暴露，用于分配 4 KiB 页表帧。

### 2.1 MMU 初始化 — 创建内核地址空间

- **`src/memory/mod.rs:39`** — `let alloc = &crate::allocator::frame::FRAME_ALLOCATOR;`
  - 用于创建内核 `AddressSpace`（根页表分配）
  - 用于 identity-map DRAM、UART、CLINT、PLIC（中间页表按需分配）
  - 用于建立内核高半区映射
- **`src/memory/mod.rs:125`** — `map_device()` 接受 `&dyn Allocator` 参数
- **`src/memory/mod.rs:170`** — `new_user_space()` 接受 `&dyn Allocator` 参数

### 2.2 AddressSpace — 页表生命周期

- **`src/memory/space.rs:35-37`** — `AddressSpace::new(alloc)` — 分配根页表帧
- **`src/memory/space.rs:56-66`** — `AddressSpace::map()` — 映射区域，按需分配中间页表
- **`src/memory/space.rs:120-124`** — `AddressSpace::destroy()` — 递归释放所有页表帧

### 2.3 PageTable — 底层帧分配/释放

- **`src/memory/table.rs:80-85`** — `PageTable::alloc_page(alloc)` — 分配 4 KiB 页表帧
- **`src/memory/table.rs:92-97`** — `PageTable::dealloc_page(alloc, pa)` — 释放页表帧
- **`src/memory/table.rs:149-171`** — `walk_mut()` — 遍历时按需分配中间页表
- **`src/memory/table.rs:206-225`** — `map_page()` — 映射单页，调用 `walk_mut`
- **`src/memory/table.rs:234-247`** — `map_region()` — 逐页映射
- **`src/memory/table.rs:276-289`** — `destroy_children()` — 递归释放子页表

> **重写注意**：新帧分配器必须实现 `core::alloc::Allocator` trait（或提供等价接口），
> 因为 MMU 模块通过 `&dyn Allocator` 使用它。`FRAME_ALLOCATOR` 的静态变量名
> 和类型签名需保持不变，或在所有引用处同步更新。

---

## 3. 全局堆分配器 (`#[global_allocator]`) 的间接触发者

以下代码通过 Rust 标准 `alloc` crate 的类型间接使用全局分配器（`Box`/`Vec`/`String`/`VecDeque` 等）。

### 3.1 调度器 — 任务栈分配

- **`src/scheduler.rs:55`** — `Vec::with_capacity(STACK_SIZE)` — 为每个新任务分配 4 KiB 栈
- **`src/scheduler.rs:8`** — `SpinLock<VecDeque<*mut TrapFrame>>` — 就绪队列（静态初始化，后续 push/pop 会触发分配）

### 3.2 陷阱处理 — 外部中断注册表

- **`src/trap.rs:22`** — `static mut EXTERNAL_HANDLERS: Option<Vec<&'static dyn IrqHandler>>`
- **`src/trap.rs:26`** — `Vec::new()` — 初始化时分配
- **`src/trap.rs:34`** — `push(handler)` — 注册中断时可能触发 realloc

### 3.3 物理帧分配器 — 位图存储

- **`src/allocator/page.rs:13`** — `use alloc::vec::Vec;` — 位图用 `Vec<u64>` 存储

### 3.4 日志 / 格式化

- 所有 `info!()`/`error!()`/`warn!()` 等宏调用 → `println!` → `format!` → 可能触发堆分配（`String` 格式化）
- 分布在：`main.rs`, `scheduler.rs`, `init.rs`, `memory/mod.rs`, `panic.rs`, `trap.rs` 等

> **重写注意**：日志格式化在分配器初始化**之后**才被调用（Phase 2），
> 但 panic handler 可能在任何时机触发——它绕过 SpinLock 直接写 UART，不依赖分配器。

---

## 4. 初始化调用链

### 4.1 启动序列

```
main()                          [src/main.rs:66]
  ├── platform::init()           [Phase 0: DTB 解析，零分配]
  └── init::run()                [src/init.rs:25]
        ├── allocator::init()    [Phase 1: bump → portal→ hybrid]
        │     ├── bump::init() + portal::switch(bump)
        │     └── hybrid::init() + portal::switch(hybrid)
        ├── driver::probe()     [Phase 1: 解析 DTB → Vec<DeviceNode>]
        ├── memory::init()          [Phase 1: 使用 frame allocator]
        ├── trap::init()         [Phase 1: 使用 Vec::new()]
        ├── driver::discover()  [Phase 2: Box::leak + hub::register]
        │     ├── plic::init() → hub::register::<Plic>("plic-0")
        │     ├── uart::init() → hub::register::<Uart>("uart-0")
        │     └── clint::init() → hub::register::<Clint>("clint-0")
        ├── PLIC.Driver::init() + register_external()
        ├── UART.Driver::init() + panic::register_uart()
        ├── print::init()        [hub::get<Uart> → &dyn Write coercion]
        ├── log::init_timestamp()
        ├── CLINT.Driver::init() + register_internal()
        ├── sie::set(SEIE)
        └── sstatus::set(SIE)
```

### 4.2 关键依赖关系

- **`allocator::init()` 必须先于 `memory::init()`**（MMU 需要 frame allocator）
- **`allocator::init()` 必须先于 `trap::init()`**（trap 用 `Vec::new()`）
- **`allocator::init()` 必须先于 `scheduler::spawn()`**（spawn 用 `Vec::with_capacity()`）
- **`platform::init()` 必须先于 `allocator::init()`**（allocator 需要 platform config 中的 DRAM 信息）
- **DTB 解析（`platform::init` / `dtb::`）零分配**，不依赖 allocator

---

## 5. `framealloc` 新模块

- **`src/allocator/framealloc.rs`** — 骨架代码（`FrameAllocator` struct + `Inner` + 链表）
- **`src/allocator/mod.rs:30`** — `pub mod framealloc;` 已声明
- 目前未被任何外部代码引用，重写时可以替换或删除

---

## 6. 重写时需要关注的文件清单

### 必须修改

| 文件 | 变更内容 |
| ------ | --------- |
| `src/allocator/mod.rs` | 新的 `init()` 入口，模块声明 |
| `src/allocator/hybrid.rs` | 新的 `#[global_allocator]` 或替换策略 |
| `src/allocator/buddy.rs` | buddy 分配器重写（或替换为其他方案） |
| `src/allocator/slab.rs` | slab 分配器重写（或替换为其他方案） |
| `src/allocator/page.rs` | 物理帧分配器重写（或替换） |
| `src/allocator/framealloc.rs` | 新帧分配器实现或删除 |

### 可能需要修改（取决于接口兼容性）

| 文件 | 变更条件 |
| ------ | --------- |
| `src/main.rs:3` | 如果不再需要 `allocator_api` feature |
| `src/memory/mod.rs:39` | 如果 `FRAME_ALLOCATOR` 的类型或名称变化 |
| `src/memory/space.rs` | 如果 `&dyn Allocator` trait 用法不变则无需改 |
| `src/memory/table.rs` | 同上 |
| `src/init.rs:28` | 如果 `allocator::init()` 签名不变则无需改 |

## memory 模块改进

### 低垂果实

- [x] **删除 `PhysPage`** — `addr.rs` 中的 `PhysPage` 类型定义未被任何代码使用，删除
- [x] **启用 `PteFlags::G`** — 内核恒等映射（DRAM、MMIO、高半区）应加 Global 位，为 ASID 做准备
- [x] **修复 `PageFault` CSR 注释** — `mtval`/`mepc` 应为 `stval`/`sepc`
- [x] **修复 `MapError::AlreadyMapped` 虚字段** — 字段定义但从未读取，改为 `()` 或删除

### 结构性改进

- [ ] **trap.rs 改用 `CURRENT_SPACE`** — 缺页时不硬编码 `KERNEL_SPACE`，从调度器 `current_root_page_number()` 获取当前空间的根页号
- [ ] **ASID 支持** — `switch_space` 改为 `sfence.vma zero, asid` 局部刷新，减少 TLB 抖动

### 能力扩展

- [ ] **Superpage 支持** — `map_region` 支持 2MB（Sv39 一级大页）和 1GB（Sv39 二级大页）
- [ ] **mmap + 缺页闭环** — 用户进程创建时调用 `add_region` + `anonymous_region` 接入实际缺页流程

### 不需要修改（仅间接依赖全局分配器）

| 文件 | 原因 |
| ------ | ------ |
| `src/scheduler.rs` | 使用 `alloc::vec::Vec` / `alloc::collections::VecDeque`，对 `#[global_allocator]` 透明 |
| `src/trap.rs` | 同上，使用 `alloc::vec::Vec` |
| `src/print.rs` | `format!` 宏间接使用 |
| `src/log.rs` | 同上 |
| `src/dtb/` | 零分配设计，完全不依赖 |
| `src/platform.rs` | 同上 |

---

## 7. `alloc` crate 类型使用汇总

这些 Rust 标准库类型在代码库中出现，全部通过 `#[global_allocator]` 使用堆内存：

| 类型 | 使用位置 | 用途 |
| ------ | --------- | ------ |
| `Vec<T>` | `scheduler.rs:55`, `trap.rs:22`, `page.rs:13` | 任务栈、中断注册表、位图 |
| `VecDeque<T>` | `scheduler.rs:8` | 调度就绪队列 |
| `format!` / `String` | 各模块日志/打印 | 格式化输出（间接） |
| `Box<T>` | `src/driver/uart.rs`, `clint.rs`, `plic.rs` | 驱动实例创建（`Box::leak` → `'static`） |

---

## 8. 设备文件抽象（devfs）

当前 hub 按具体类型 + 名字管理设备实例，未来应抽象为统一文件接口。

### 目标

```
现在:                         未来:
hub::get::<Uart>("uart-0")    devfs / uart0 → fd read/write
hub::get::<Plic>("plic-0")    devfs / plic → ioctl/mmap
hub::get::<Clint>("clint-0")  devfs / clint0 → ioctl
```

### 设计要点

- **hub 作为 devfs 后端**——devfs 的 open 实现包装 `hub::get`
- **trait 对象放在文件层**——`dyn Read/Write/Seek/Ioctl` 在 devfs/file 侧，不在 hub 侧
- **名字天然兼容**——`"uart-0"` → `/dev/uart0`
- **hub 的 `replace`/`unregister`/`for_each`/`count` 暂保留**——devfs 挂载/卸载/遍历需要

### 拆分阶段

1. ~~hub 去掉 fat pointer，只存具体类型~~ ✅ 已完成
2. 设计 devfs 文件 trait（`Read`/`Write`/`Ioctl` 等）
3. devfs 挂载 + open/close 实现
4. 各驱动实现文件 trait
5. print 通过 fd 输出

### 设计确认

- **同一驱动文件可管理同 compatible 的多个实例**——`uart.rs` 管所有 NS16550A，每个实例由 `(base, interrupt)` 区分，方法通过 `&self` 操作各自 MMIO 区域，无单例状态。兼容不同型号的设备才需新驱动文件。

---

## 9. 结构性 & API 设计问题（2026-07-31 审查）

### 🔴 严重

- [x] **`Driver` trait 的 `init()` 返回类型太弱** (`src/driver/traits.rs:3`)
  - `Result<(), &'static str>` 丢失结构化错误信息，`DriverError::Init` 包裹了字符串但字段从未被读取
  - **修复**: 定义 `DriverInitError` 枚举，或让 `DriverError::Init` 保留更多上下文

- [x] **`map_devices()` 静默丢弃映射错误** (`src/driver/mod.rs`)
  - 当前用 `let _ = ks.map(...)` 丢弃所有错误（`NotAligned`、`OutOfMemory` 等）。size 取整已修复，但其他错误仍被忽略
  - **修复**: 至少对失败的设备做 `warn!("mapping failed for {}: {:?}", dev.compatible, result)`

- [x] **`PageTable::map()` 对齐要求未在类型层面表达** (`src/memory/table.rs:152-175`)
  - `size` 参数是 `usize`，调用者容易忘记页对齐要求（如 UART `size=0x100` bug）
  - **修复**: 提供 `map_aligned()` / `map_rounding()` 包装函数自动处理对齐，或引入 `AlignedSize` newtype

### 🟡 中等

- [ ] **`Driver` trait 中 `compatible()` 从未被调用** (`src/driver/traits.rs:2`)
  - 设备匹配在 `driver::init()` 中通过硬编码 `match dev.compatible` 完成，而非 trait 分发
  - **修复**: 要么删除 `compatible()`，要么构建 `compatible → init_fn` 注册表驱动匹配

- [ ] **hub 和设备注册的三层 init 模式冗余** (`src/driver/mod.rs`, 各驱动文件)
  - 每个设备需经历：①模块级 `xxx::init()` 构造+leak → ②`hub::register()` → ③`dev.init()` (Driver trait) 硬件初始化
  - **修复**: 让 Driver trait 提供 `create_and_register()` 统一入口，减少样板

- [ ] **`flush_tlb()` 调用策略不一致** (`src/memory/space.rs`, `src/memory/table.rs`)
  - `space::init()` 末尾调用了 `flush_tlb()`，但 `map()`/`page_fault()`/`protect()` 等修改 PTE 的操作都没有
  - **修复**: 将 TLB 刷新集成到 `AddressSpace` 方法中（在 `map`/`unmap`/`protect` 成功后自动 `sfence.vma`），或明确文档化"调用者负责"契约

- [ ] **`PageTable::walk_mut` 物理地址→虚拟指针的隐式假设** (`src/memory/table.rs:124`)
  - `unsafe { &mut *(l2.paddr() as *mut PageTable) }` 假设所有物理地址 identity-mapped，无文档无断言
  - **修复**: 添加 `# Safety` 文档，debug build 中加入地址范围断言

### 🟢 低优先级

- [ ] **`RelLock` 在中断已关闭的上下文中的冗余 TrapGuard** (`src/lock/reentrant.rs:60-61`)
  - 初始化期间中断未使能，`TrapGuard::save()` 保存/恢复 SIE 是冗余的 CSR 操作
  - **修复**: 提供 `lock_noirq()` 优化路径，或文档化当前行为

- [ ] **设备发现 `probe()` 被调用两次时静默回退** (`src/driver/tree.rs:37-38`)
  - `OnceLock::set` 失败时只打 warn，调用者不会得到错误返回值
  - **修复**: 考虑 panic 或返回 `Result`（重复调用意味逻辑错误）

- [ ] **`ExternalInterrupt::init()` 无错误返回** (`hal/interrupt.rs:45`)
  - 返回 `()`，无法报告 PLIC 硬件初始化失败；`Plic::Driver::init()` 在外层总是 `Ok(())`
  - **修复**: 改为 `fn init(&self) -> Result<(), &'static str>`

- [ ] **`Uart::write_byte()` 安全函数包装了 unsafe MMIO 操作** (`driver/uart.rs:61-64`)
  - 调用者无法从签名知道必须先映射 MMIO。`map_devices()` 失败后调用此函数直接触发 page fault
  - **修复**: 标记为 `unsafe fn` 并添加 `# Safety` 文档，或引入 `MappedMmio` 类型证明

- [ ] **`Driver::init()` 的 `transmute` 绕过了 `'static` 生命周期** (`driver/uart.rs:106-107`)
  - `trap::register_interrupt_handler` 需要 `&'static dyn InterruptHandler`，通过 `core::mem::transmute(self)` 绕过
  - **修复**: 将 Driver trait 改为 `fn init(&'static self) -> ...` 消除 transmute

- [ ] **`DeviceNode::base` 是裸 `usize` 而非 `PhysAddr`** (`driver/tree.rs:17`)
  - 在 `driver/mod.rs` 中既被转为 `VirtAddr` 又被转为 `PhysAddr`，类型系统无法防止误用
  - **修复**: 改为 `pub base: PhysAddr`

- [ ] **`VirtAddr::new_truncate` / `PhysAddr::from_raw` 命名不一致** (`memory/addr.rs`)
  - `VirtAddr` 用 `new_truncate()`，`PhysAddr` 用 `from_raw()` — 同操作不同名
  - **修复**: 统一为一种命名约定（建议都用 `new_truncate` 或 `from_raw`）

- [ ] **`walk_ref` 返回 `Option`，`walk_mut` 返回 `Result` — 不对称** (`memory/table.rs:77,107`)
  - `walk_ref` 把所有失败折叠为 `None`（无法区分 "中间表缺失" vs "叶子无效"）
  - **修复**: `walk_ref` 也返回 `Result<(PhysAddr, PteFlags), MapError>`

- [ ] **所有驱动模块级 `init()` 与 Driver trait `init()` 命名冲突** (多文件)
  - `plic::init(base, ctx)` (构造) 和 `plic.init()` (trait 硬件初始化) 同名但语义完全不同
  - **修复**: 工厂函数改名 `create()`：`uart::create(base, irq)`, `plic::create(base, ctx)` 等

- [ ] **PTE 指针解引用缺乏 `SAFETY` 注释** (`memory/table.rs:82,88,124,138,187,193`)
  - 6 处 `unsafe { &mut *(l2.paddr() as *mut PageTable) }` 无一有 `// SAFETY:` 文档
  - **修复**: 每处添加 "SAFETY: PTE 已检查 is_valid() && !is_leaf()，paddr() 指向有效 PageTable 帧"

- [ ] **`hub::get()`/`for_each()` 的裸指针重建缺 SAFETY 注释** (`driver/hub.rs:35,68`)
  - `unsafe { &*(entry.ptr as *const T) }` 无文档，依赖隐式约定
  - **修复**: 添加 "SAFETY: register 存的是 &'static T，TypeId 匹配到位，引用永不失效"

- [ ] **`init::run()` 是 `pub unsafe fn` 但没有 `# Safety` 文档** (`init.rs:39`)
  - 调用者（`main`）无合同可知需保证什么前置条件
  - **修复**: 文档化：单 hart、中断关闭、satp=bare、栈 identity-mapped 等

- [ ] **`unsafe impl Sync` 缺注释（Plic）或不一致的多语言注释（Uart/Clint）** (`driver/plic.rs:30`, `uart.rs:16`, `clint.rs:23`)
  - Plic 完全无注释；Uart 中文、Clint 中文 — 应当统一为英文 `// SAFETY:`
  - **修复**: 统一英文 SAFETY 注释，说明单 hart 内核，多 hart 需要重新评估

- [ ] **`tree::probe_devices` 中 `transmute` 延长生命周期** (`driver/tree.rs:66`)
  - `core::mem::transmute(compatible)` 将 DTB 生命周期的 `&str` 转成 `&'static str`
  - **修复**: 复制字符串到内核持有缓冲区，或解释为什么 DTB 物理内存永久有效

- [ ] **`INTERRUPT_HANDLERS` 稀疏数组无上限** (`trap.rs:25`)
  - PLIC 中断号可达 1023，`Vec::resize` O(n) 且无上限保护，bug 可能导致巨量分配
  - **修复**: 加 `const MAX_INTERRUPTS: usize = 256` 或换 `BTreeMap`

- [ ] **`OnceLock` drop 注释误导** (`lock/once.rs:123-126`)
  - 注释说 "kernel 全局变量永不需要 Drop"，但 `get_or_init()` 在竞态丢弃时会 call `drop()`
  - **修复**: 澄清注释，或实现完整 Drop

- [ ] **未文档化的锁层次** (所有 lock 模块)
  - `driver::init()` 中交叉获取 `RwLock`(hub) 和 `SpinLock`(handlers)，顺序靠运气保证
  - **修复**: 在模块顶部文档化锁获取层次：

    ```
    // Lock hierarchy: 1. KERNEL_SPACE (RelLock) → 2. hub::TABLE (RwLock) → 3. INTERRUPT_HANDLERS (SpinLock)
    ```
