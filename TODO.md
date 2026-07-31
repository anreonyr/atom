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

### P2.3 `drivers::tree::probe()` — 静默丢弃 → `warn!`

- **文件**: `src/drivers/tree.rs:37`
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
