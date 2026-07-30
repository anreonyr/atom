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

- **`src/mmu/mod.rs:39`** — `let alloc = &crate::allocator::frame::FRAME_ALLOCATOR;`
  - 用于创建内核 `AddressSpace`（根页表分配）
  - 用于 identity-map DRAM、UART、CLINT、PLIC（中间页表按需分配）
  - 用于建立内核高半区映射
- **`src/mmu/mod.rs:125`** — `map_device()` 接受 `&dyn Allocator` 参数
- **`src/mmu/mod.rs:170`** — `new_user_space()` 接受 `&dyn Allocator` 参数

### 2.2 AddressSpace — 页表生命周期

- **`src/mmu/space.rs:35-37`** — `AddressSpace::new(alloc)` — 分配根页表帧
- **`src/mmu/space.rs:56-66`** — `AddressSpace::map()` — 映射区域，按需分配中间页表
- **`src/mmu/space.rs:120-124`** — `AddressSpace::destroy()` — 递归释放所有页表帧

### 2.3 PageTable — 底层帧分配/释放

- **`src/mmu/table.rs:80-85`** — `PageTable::alloc_page(alloc)` — 分配 4 KiB 页表帧
- **`src/mmu/table.rs:92-97`** — `PageTable::dealloc_page(alloc, pa)` — 释放页表帧
- **`src/mmu/table.rs:149-171`** — `walk_mut()` — 遍历时按需分配中间页表
- **`src/mmu/table.rs:206-225`** — `map_page()` — 映射单页，调用 `walk_mut`
- **`src/mmu/table.rs:234-247`** — `map_region()` — 逐页映射
- **`src/mmu/table.rs:276-289`** — `destroy_children()` — 递归释放子页表

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
- 分布在：`main.rs`, `scheduler.rs`, `init.rs`, `mmu/mod.rs`, `panic.rs`, `trap.rs` 等

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
        ├── drivers::probe()     [Phase 1: 解析 DTB → Vec<DeviceNode>]
        ├── mmu::init()          [Phase 1: 使用 frame allocator]
        ├── trap::init()         [Phase 1: 使用 Vec::new()]
        ├── drivers::discover()  [Phase 2: Box::leak + hub::register]
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

- **`allocator::init()` 必须先于 `mmu::init()`**（MMU 需要 frame allocator）
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
| `src/mmu/mod.rs:39` | 如果 `FRAME_ALLOCATOR` 的类型或名称变化 |
| `src/mmu/space.rs` | 如果 `&dyn Allocator` trait 用法不变则无需改 |
| `src/mmu/table.rs` | 同上 |
| `src/init.rs:28` | 如果 `allocator::init()` 签名不变则无需改 |

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
| `Box<T>` | `src/drivers/uart.rs`, `clint.rs`, `plic.rs` | 驱动实例创建（`Box::leak` → `'static`） |

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
