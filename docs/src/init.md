# init 模块 API 契约

> 本文件是 `src/init.rs`（+ `main.rs` 顶层装配）的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Boot sequence」迁出。

## 1. 职责

平台启动序列编排（顶层组合模块）：组装 allocator → 空间 → trap → 驱动 → VFS/console/log → 时钟/中断，
最后回到 `main` 起调度。`init.rs` 保留顶层。

边界：

- **只做编排**：各子系统的初始化细节归各自模块（如驱动探测见 `driver.md`、VFS 建树见 `file.md`）。
- **sie 使能唯一入口**：所有 `sie` enables 集中在 Phase 3，驱动不碰 `sie`。

## 2. 引导流程

```
QEMU → OpenSBI (M-mode) → mret → _start → early() → main → init::run()
```

`init::run()` phases：

1. **Phase 1** (allocator ready)：`allocator::init()` (bump → hybrid) → `space::init()` →
   `runtime::trap::init()` (stvec 先就位，driver 探测期间异常可进 trap_handler，而非重入 `_start`) →
   `driver::init()` (hub: DTB discover → match → deferred probe; PLIC/UART/CLINT all initialized here)。
2. **Phase 2** (VFS + console + log)：注册标准流（`registry::register` /dev/stdin、/dev/stdout）→
   `create_devfs()`（枚举 registry 建 /dev 节点：null/stdin/stdout/zero/consoleN/uartN +
   `/dev/console → consoleN` 符号链接）+ `filetable::set_root()` +
   stdio 预置（fd 0=stdin、fd 1=stdout）→ `log::init_timestamp()` → `log::set_max_level()`。
   输出目标由 `/dev/console` 符号链接表达：Phase 1 中首个终端 probe 注册，devfs 构建时
   链接指向它；此间（root 未建）print 解析链接失败回落 SBI，无显式 print::init。
3. **Phase 3**：`clock::tick::start()`（首次装载 10ms 定时中断；原 CLINT probe 装载已上移至此）→
   `sie::set(SEIE)` + `sie::set(STIE)` → `sstatus::set(SIE)`。All `sie` enables live here (single entry point)。

Back in `main`：`schedule::spawn(Entry::Kernel(task_a), None)` → WFI idle loop
（task_a 等演示任务定义于顶层 `demos.rs`——main 的抽离，编译期 `DEMO_*` 开关）。

## 3. 公共 API 面

### 3.1 函数

| API | 签名 | 语义 |
|-----|------|------|
| `init::run` | `pub fn run()` | 执行 Phase 1/2/3 平台启动序列 |
| `main` | （`_start` asm → `early()` → `main`） | 顶层装配：早期初始化 + `init::run` + 起调度 + WFI |

### 3.2 数据 / 常量

- 启动序列无长期全局态；输出目标由 `/dev/console` 符号链接表达（`file::io::print` 解析链接 → 终端 File）。

## 4. 命名框架

- 顶层组合模块不新增独立约定；编排各子系统时遵循其各自契约。

## 5. 生命周期与并发

- 启动为单 hart 串行过程；`sie`/`sstatus` 使能在 Phase 3 集中完成（唯一入口），此后中断开启。
- 时钟 tick 装载在 Phase 3（原 CLINT probe 装载已上移至此）。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（Boot sequence）。
