# TODO — 错误处理改进

> 审计日期: 2026-07-31
> 审计范围: 55 个 `.rs` 文件
> 总发现: 7 `.unwrap()` · 27 `.expect()` · 6 `panic!`/`unreachable!` · 9 处丢弃 Result

## 设计决策

- **新增 `init::Error`**: 遵循项目模式（模块路径即命名空间，如 `filesystem::Error`），用于 `init::run()` 返回值
- **`get_internal/get_external` → `Option`**: 移除中断热路径上的 panic，分支开销可忽略
- **`register_*` → `warn!`**: 双重注册是编程错误，日志记录即可（匹配 `log::init_timestamp` 的 `.ok()` 模式）
- **分配器 init 链不改**: `#[global_allocator]` 契约下 Vec 分配失败会直接 panic，Result 无法传播

---

## P0 — 运行时 panic 路径

### P0.1 `hal::interrupt::get_internal` / `get_external` → `Option`

- **文件**: `src/hal/interrupt.rs:33,62`
- **当前**: `pub fn get_internal() -> &'static dyn InternalInterrupt` — 未注册时 panic
- **目标**: `pub fn get_internal() -> Option<&'static dyn InternalInterrupt>`
- **调用方**: `trap.rs:188,193` — 改为 `if let Some(...)` 守卫
- **状态**: ☑

### P0.2 缺页异常 panic → `warn!` + 任务终止

- **文件**: `src/trap.rs:240`
- **当前**: `panic!("unhandled page fault: {:?}", fault)` — 用户态缺页导致内核崩溃
- **目标**: `warn!` + 若为用户空间则终止当前任务；若为内核空间则记录并返回
- **注意**: 任务终止机制尚未实现，先加 `warn!` + TODO
- **状态**: ☑

---

## P1 — 启动期错误传播（Result 返回值）

### P1.1 `memory::space::init()` → `Result<(), MapError>`

- **文件**: `src/memory/space.rs:326`
- **当前**: `pub unsafe fn init()` — 4 处 `.expect()` 包裹 `MapError`
- **目标**: `pub unsafe fn init() -> Result<(), MapError>` — `.expect()` → `?`
- **调用方**: `init.rs:31` — 改为 `memory::space::init()?;`
- **状态**: ☑

### P1.2 `memory::map_device()` → `Result<(), MapError>`

- **文件**: `src/memory/mod.rs:59`
- **当前**: `.expect("map_device failed")` + 内核空间 None 时静默忽略
- **目标**: `Result<(), MapError>` — `kernel_space` None 时返回 `Err`
- **调用方**: 无（新增 public API）
- **状态**: ☑

### P1.3 新增 `init::Error` 枚举

- **文件**: `src/init.rs`
- **目标**: 定义 5 变体的 `Error` 枚举 + `type Result<T>` 别名
- **变体**: `Memory(MapError)`, `DriverNotFound(&'static str)`, `DriverInit(DriverError)`, `AlreadyInitialized(&'static str)`, `DtbMissing`
- **状态**: ☑

### P1.4 `init::run()` → `Result<(), init::Error>`

- **文件**: `src/init.rs:25` + `src/main.rs:67`
- **当前**: `pub fn run()` — 8+ 处 `.expect()`
- **目标**: `pub fn run() -> Result<(), Error>` — `.expect()` → `.map_err()` + `?`
- **调用方**: `main.rs` — 改为 `init::run().expect("kernel boot failed");`
- **依赖**: P1.1, P1.3
- **状态**: ☑

---

## P2 — 诊断与加固

### P2.1 `register_internal` / `register_external` — panic → `warn!`

- **文件**: `src/hal/interrupt.rs:26,55`
- **当前**: `panic!("internal interrupt already registered")`
- **目标**: `.set(t).ok();` + `warn!("...")` 模式
- **状态**: ☑

### P2.2 `filetable::set_root()` — panic → `warn!`

- **文件**: `src/filesystem/filetable.rs:49-53`
- **当前**: `.expect("filesystem: root already initialized")`
- **目标**: `.ok()` + `warn!("...")` 模式
- **状态**: ☑

### P2.3 `driver::tree::probe()` — 静默丢弃 → `warn!`

- **文件**: `src/driver/tree.rs:37`
- **当前**: `let _ = DEVICES.set(devices);`（失败时无诊断）
- **目标**: `if DEVICES.set(devices).is_err() { warn!(...) }`
- **状态**: ☑

### P2.4 `block::init()` — 静默丢弃 → `warn!`

- **文件**: `src/memory/allocator/block.rs:259`
- **当前**: `BLOCK_ALLOCATORS.set(allocators).ok();`（失败时无诊断）
- **目标**: `if ... .is_err() { warn!(...) }`
- **状态**: ☑

### P2.5 `memory::fault::PageFault::capture()` — `unreachable!()` → `panic!()`

- **文件**: `src/memory/fault.rs:52`
- **当前**: `unreachable!("capture() called on non-page-fault scause={}", code)`
- **问题**: release build 中 `unreachable!()` 是 UB
- **目标**: `panic!("capture() called on non-page-fault scause={}", code)`
- **状态**: ☑

### P2.6 `table.rs:211` — Drop 中 unwrap → 空指针安全处理

- **文件**: `src/memory/table.rs:211`
- **当前**: `NonNull::new(entry.paddr() as *mut PageTable).unwrap()`
- **问题**: Drop 中 panic 会导致 double-panic → abort
- **目标**: `let Some(child_ptr) = ... else { continue; }`
- **状态**: ☑

---

## 实施顺序

```
 1. P2.5  (1 词, 零风险, 零依赖)
 2. P2.6  (2 行, 零依赖)
 3. P0.1  (2 签名 + 2 调用方)
 4. P0.2  (1 行)
 5. P2.3  (1 行)
 6. P2.4  (1 行)
 7. P2.2  (1 行)
 8. P2.1  (2 行)
 9. P1.1  (签名 + 4 expect→?)       ← 被 P1.4 依赖
10. P1.2  (签名, 0 调用方)          ← 被 P1.4 依赖
11. P1.3  (init::Error 枚举)        ← 被 P1.4 依赖
12. P1.4  (8+ expect 转换 + main.rs)  ← 依赖 9-11
```

## 可加 Result 返回值的模块汇总

| 模块 | 函数 | 当前签名 | 新签名 | 错误类型 |
|------|------|---------|--------|---------|
| `memory::space` | `init()` | `pub unsafe fn init()` | `pub unsafe fn init() -> Result<(), MapError>` | `MapError` (已有) |
| `memory` | `map_device()` | `pub unsafe fn map_device(...)` | `pub unsafe fn map_device(...) -> Result<(), MapError>` | `MapError` (已有) |
| `init` | `run()` | `pub fn run()` | `pub fn run() -> Result<(), Error>` | `init::Error` (新增) |

---

# 代码质量审计 — 模块级问题汇总

> 审计日期: 2026-07-31
> 来源: 全量代码审查
> 分类: 🟡 中等 / 🟢 低优先级

---

## `src/driver/traits.rs` — Driver trait 定义

### 🟡 D-T1 `compatible()` 从未被调用 ☑

- **当前**: trait 定义了 `fn compatible() -> &'static str`，但设备匹配在 `driver::init()` 中通过硬编码 `match dev.compatible` 完成，完全不经过 trait 分发
- **修复**: 要么删除 `compatible()`，要么构建 `compatible → init_fn` 注册表驱动匹配
- **状态**: ☑ (已从 trait 和全部 3 个 impl 中删除)

### 🟢 D-T2 `Driver::init()` 签名导致 `transmute` 绕过生命周期 ☑

- **当前**: `fn init(&self) -> Result<(), DriverError>`，实现方（uart.rs:106-107）通过 `transmute(self)` 转为 `&'static dyn InterruptHandler` 以注册到 trap
- **修复**: 改为 `fn init(&'static self) -> Result<(), DriverError>` 消除 transmute
- **状态**: ☑ (trait + 3 impl 均改为 &'static self; uart.rs 中的 transmute 已移除)

---

## `src/driver/mod.rs` — 驱动发现与编排

### 🟡 D-M1 三层 init 模式冗余 ☑

- **当前**: 每个设备需经历 ①模块级 `create()` 构造+leak → ②`hub::register()` → ③`dev.init()` (Driver trait) 硬件初始化。样板重复
- **修复**: 让 Driver trait 提供 `create_and_register()` 统一入口
- **状态**: ☑ (每个驱动提供 `register()` 函数合并 create+hub::register; mod.rs 中每设备从 2 行缩减为 1 行)

### 🟢 D-M2 模块级 `init()` vs trait `init()` 命名冲突 ☑

- **当前**: `plic::init(base, ctx)` (工厂构造) 和 `plic.init()` (trait 硬件初始化) 同名但语义完全不同；`uart::init` / `clint::init` 同理
- **修复**: 工厂函数改名 `create()`：`uart::create(base, irq)`, `plic::create(base, ctx)` 等
- **状态**: ☑ (全部 3 个模块的工厂函数已重命名为 create(); mod.rs 调用方已更新)

---

## `src/driver/tree.rs` — DTB 设备发现

### 🟢 D-TR1 `DeviceNode::base` 是裸 `usize` 非 `PhysAddr` ☑

- **当前**: 在 `mod.rs` 中既被转为 `VirtAddr` 又被转为 `PhysAddr`，类型系统无法防止误用
- **修复**: 改为 `pub base: PhysAddr`
- **状态**: ☑ (DeviceNode.base → PhysAddr; 添加 LowerHex impl; 更新全部调用方)

### 🟢 D-TR2 `probe()` 被调用两次时静默回退 ☑

- **当前**: `OnceLock::set` 失败时只打 `warn!`，调用者得不到错误返回值
- **修复**: 考虑 panic（重复调用意味逻辑错误）或返回 `Result`
- **状态**: ☑ (warn! → panic!，重复调用视为逻辑错误)

### 🟢 D-TR3 `transmute` 延长生命周期 ☑

- **当前**: `tree.rs:66` — `transmute(compatible)` 将 DTB 生命周期的 `&str` 转成 `&'static str`
- **修复**: 复制字符串到内核持有缓冲区，或添加 SAFETY 注释解释为什么 DTB 物理内存永久有效
- **状态**: ☑ (已添加英文 SAFETY 注释说明 DTB 物理内存由 OpenSBI 保留且永不被释放)

---

## `src/driver/uart.rs` — NS16550A UART

### 🟢 D-U1 `write_byte()` 安全函数包装 unsafe MMIO ☑

- **当前**: 调用者无法从签名知道必须先映射 MMIO；`map_devices()` 失败后调用此函数直接 page fault
- **修复**: 标记为 `unsafe fn` 并添加 `# Safety` 文档，或引入 `MappedMmio` 证明类型
- **状态**: ☑ (标记为 unsafe fn + # Safety 文档；所有 8 处调用方加 SAFETY 注释)

### 🟢 D-U2 `unsafe impl Sync` 注释语言不一致 ☑

- **当前**: Uart 用中文，Clint 用中文，Plic 完全无注释。应统一为英文 `// SAFETY:`
- **修复**: 统一英文 SAFETY 注释，说明单 hart 内核
- **状态**: ☑ (Uart/Clint/Plic 的 Sync impl 均已统一为英文 `// SAFETY: single-hart kernel`)

---

## `src/driver/plic.rs` — PLIC 控制器

### 🟢 D-P1 `unsafe impl Sync` 缺注释 ☑

- **当前**: 完全无 SAFETY 文档说明为什么单 hart 下安全
- **修复**: 添加英文 `// SAFETY:` 注释
- **状态**: ☑

---

## `src/driver/hub.rs` — 设备注册表

### 🟢 D-H1 `get()`/`for_each()` 裸指针重建缺 SAFETY 注释 ☑

- **当前**: `hub.rs:35,68` — `unsafe { &*(entry.ptr as *const T) }` 无文档，依赖隐式约定
- **修复**: 添加 `// SAFETY: register 存的是 &'static T，TypeId 匹配到位，引用永不失效`
- **状态**: ☑

---

## `src/memory/table.rs` — Sv39 页表

### 🟡 M-T1 `walk_mut` 物理地址→虚拟指针的隐式假设 ☑

- **当前**: `table.rs:124` — `unsafe { &mut *(l2.paddr() as *mut PageTable) }` 假设所有物理地址 identity-mapped，无文档无断言
- **修复**: 添加 `# Safety` 文档，debug build 中加入地址范围断言
- **状态**: ☑ (已在 walk_mut 文档中添加 "Physical-to-virtual assumption" 小节)

### 🟢 M-T2 `walk_ref` 返回 `Option`，`walk_mut` 返回 `Result` — 不对称 ☑

- **当前**: `walk_ref` 把所有失败折叠为 `None`（无法区分"中间表缺失" vs "叶子无效"）
- **修复**: `walk_ref` 也返回 `Result<(PhysAddr, PteFlags), MapError>`
- **状态**: ☑ (改为 Result<...>; translate() 通过 .ok() 适配)

### 🟢 M-T3 6 处 PTE 指针解引用缺乏 `SAFETY` 注释 ☑

- **当前**: `table.rs:82,88,124,138,187,193` — 无一有 `// SAFETY:` 文档
- **修复**: 每处添加 "SAFETY: PTE 已检查 is_valid() && !is_leaf()，paddr() 指向有效 PageTable 帧"
- **状态**: ☑ (walk_ref ×2, walk_mut ×2, unmap ×2 均已添加英文 SAFETY 注释)

---

## `src/memory/space.rs` — AddressSpace

### 🟡 M-S1 `flush_tlb()` 调用策略不一致 ☑

- **当前**: `space::init()` 末尾调用了 `flush_tlb()`，但 `map()`/`page_fault()`/`protect()` 等修改 PTE 的操作都没有
- **修复**: 要么每个修改操作后自动 `sfence.vma`，要么明确文档化"调用者负责"契约
- **状态**: ☑ (已在 AddressSpace::map() / map_region() / unmap() / protect() 末尾自动调用 flush_tlb())

### 🟢 M-S2 `init::run()` 是 `pub unsafe fn` 但无 `# Safety` 文档 ☑

- **文件**: `src/init.rs:39`
- **当前**: 调用者（`main`）无合同可知需保证什么前置条件
- **修复**: 文档化：单 hart、中断关闭、satp=bare、栈 identity-mapped 等
- **状态**: ☑ (已添加英文 `# Safety` 文档，列出全部前置条件)

---

## `src/memory/addr.rs` — 地址类型

### 🟢 M-A1 `VirtAddr::new_truncate` / `PhysAddr::from_raw` 命名不一致 ☑

- **当前**: 同操作（从 usize 构造地址）不同名
- **修复**: 统一为一种命名约定（建议都用 `new_truncate` 或 `from_raw`）
- **状态**: ☑ (统一为 `from_raw`: `VirtAddr::from_raw` + `PhysAddr::from_raw`)

---

## `src/lock/reentrant.rs` — RelLock

### 🟢 L-R1 初始化期间 TrapGuard 冗余 ☑

- **当前**: `reentrant.rs:60-61` — 引导阶段中断未使能，`TrapGuard::save()` 保存/恢复 SIE 是冗余 CSR 操作
- **修复**: 提供 `lock_noirq()` 优化路径，或文档化当前行为
- **状态**: ☑ (已在 `lock()` 文档中注明: TrapGuard 在引导期冗余，接受此开销以保持简洁)

---

## `src/lock/once.rs` — OnceLock

### 🟢 L-O1 Drop 注释误导 ☑

- **当前**: `once.rs:123-126` — 注释说"kernel 全局变量永不需要 Drop"，但 `get_or_init()` 在竞态丢弃时会 call `drop()`
- **修复**: 澄清注释，或实现完整 Drop
- **状态**: ☑ (已澄清: 无 Drop impl 是因为内核全局静态生存到系统复位; 同时说明 get_or_init() 竞态丢弃的语义)

---

## `src/hal/interrupt.rs` — 中断 trait

### 🟢 H-I1 `ExternalInterrupt::init()` 无错误返回 ☑

- **当前**: `hal/interrupt.rs:45` — 返回 `()`，无法报告 PLIC 硬件初始化失败
- **修复**: 改为 `fn init(&self) -> Result<(), &'static str>`
- **状态**: ☑ (trait + Plic impl 均已改为 Result; Driver::init 通过 map_err 传播)

---

## `src/trap.rs` — 中断/异常分发

### 🟢 TR1 `INTERRUPT_HANDLERS` 稀疏数组无上限 ☑

- **当前**: `trap.rs:25` — PLIC 中断号可达 1023，`Vec::resize` O(n) 且无上限保护
- **修复**: 加 `const MAX_INTERRUPTS: usize = 256` 或换 `BTreeMap`
- **状态**: ☑ (添加 MAX_INTERRUPTS=256 常量 + assert 守卫)

---

## 跨模块

### 🟢 X1 未文档化的锁层次 ☑

- **当前**: 所有 lock 模块 — `driver::init()` 中交叉获取 `RwLock`(hub) 和 `SpinLock`(handlers)，顺序靠运气保证
- **修复**: 在模块顶部文档化锁获取层次：
  ```
  // Lock hierarchy: 1. KERNEL_SPACE (RelLock) → 2. hub::TABLE (RwLock) → 3. INTERRUPT_HANDLERS (SpinLock)
  ```
- **状态**: ☑ (已在 `src/lock/mod.rs` 模块文档中添加 `# Lock hierarchy` 小节)

---

## 实施顺序（建议）

```
 1. M-T3  (SAFETY 注释，零风险)
 2. D-H1  (SAFETY 注释)
 3. D-U2  (统一注释语言)
 4. D-P1  (补 SAFETY 注释)
 5. D-TR3 (transmute → 文档/复制)
 6. M-S2  (init::run # Safety 文档)
 7. X1    (锁层次文档)
 8. L-O1  (OnceLock Drop 注释)
 9. L-R1  (RelLock 文档/优化路径)
10. M-A1  (命名统一)
11. M-T2  (walk_ref → Result)
12. D-M2  (工厂函数 rename)
13. D-TR1 (DeviceNode::base 类型)
14. H-I1  (ExternalInterrupt::init → Result)
15. D-U1  (write_byte → unsafe)
16. TR1   (INTERRUPT_HANDLERS 上限)
17. D-TR2 (probe 重复调用)
18. D-T1  (compatible() 删除或注册表)
19. M-S1  (flush_tlb 一致性)
20. D-T2  (Driver::init &'static self)
21. D-M1  (三层 init 统一入口)
```
