# file 模块 API 契约

> 本文件是 `src/file/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「VFS layer (file/)」迁出。

## 1. 职责

文件域多路复用器：顶层契约 `ops` 约束全部文件实现，不反向依赖任何提供方（log/driver 只注册、只消费契约）。
VFS 遵循 Linux **file_operations / file / inode** 切分。

边界：

- **不做具体文件系统**：fat32 等未来实现同样受 `ops.rs` 顶层契约约束。
- **不反向依赖提供方**：设备/驱动只注册到 registry，file 域不 import 它们。
- **UART 驱动层看不到 `File` 类型**：契约层在 `file::io::console` 构造视图，driver 不碰 `File`。

## 2. 引导流程（init Phase 2）

```
registry::register(/dev/stdin) + registry::register(/dev/stdout)   ← io::stdio::STDIN/STDOUT
终端注册（Phase 1 driver probe）：io::console::register → /dev/consoleN + /dev/uartN
create_devfs()   ← 遍历 registry::all() 建 /dev 子树（null/stdin/stdout/zero/consoleN/uartN
                   + /dev/console → 首个 consoleN 符号链接；内建 null/zero 幂等自注册）
filetable::set_root()
stdio 预置：fd 0=/dev/stdin（挂 Stdin）、fd 1=/dev/stdout（挂 Stdout）
```

## 3. 公共 API 面

### 3.1 文件能力契约（ops.rs — 零依赖叶子）

- **`File`** — 单一能力 trait（Linux `file_operations`）：`read(offset, buf)` / `write(offset, buf)` /
  `seek(pos, current)` / `control()`；未实现操作默认 `NotSupported`。定义在 `crate::file::ops`（契约随域归位），
  driver 侧与 VFS 侧都依赖它。
- **`Read`/`Write`** — std::io 风格流契约：`Stdin: Read`、`Stdout: Write`（阻塞句柄视图）。
- **`FileError`/`OpenFlags`/`SeekFrom`** — Linux fs.h 对应物。

实现者：终端核心（`file::io::console::Console`，/dev/consoleN；`RawFile`，/dev/uartN）、io 标准流（`Stdin`/`Stdout`）、devfs 节点（`NullDev`/`ZeroDev`）、未来的具体文件系统。

### 3.2 registry.rs（复用器核心）

- `register(name, &'static dyn File)` / `all()` — SpinLock 表；devfs 枚举建节点。
- 设备注册点：内建 null/zero、标准流 stdin/stdout（init.rs 注册 `io::stdio::STDIN/STDOUT`）、终端（`io::console::register` 注册 `/dev/consoleN` + `/dev/uartN`）。

### 3.3 VFS 框架（vfs/）

- **`Inode`**（inode.rs）— 命名节点：`file: Option<&'static dyn File>`（None = 目录）+ `children` +
  `target: Option<&'static str>`（Symlink 节点指向同目录相对名）。
  经 `InodeBuilder` + `Box::leak` 引导期构建，之后不可变；`lookup` 逐组件解析并跟随 symlink
  （`follow`，同目录相对解析，深度上限防环）；`filetable::resolve(path)` 返回目标 `&'static dyn File`（零锁）。
- **`OpenFile` / `filetable`**（filetable.rs）— fd → OpenFile 全局表（`RwLock`）。
  - **offset ownership**：偏移在 `OpenFile`（per-open-descriptor，Linux `struct file` 的 `f_pos`），**不在设备里**。
    VFS 把 `offset` 传入每次 `read`/`write`；`filetable::seek` 委托 `File::seek(pos, current)`（Linux `llseek`）
    返回新绝对偏移，VFS 存回 `OpenFile::offset`。**没有 `FileSeek` trait**。
  - `filetable::read`/`write` 先强制 fd 访问模式（`is_readable`/`is_writable` → `PermissionDenied`）再进设备。

### 3.4 devfs（纯枚举器）

- `create_devfs()` 遍历 `registry::all()` 建 /dev 子树（内建 null/zero）。
- `/dev/log` 因 log → file::io 输出依赖成环而**暂移除**（`log::log_read` 预留，未来经 registry 恢复）。

### 3.5 io（终端标准流域，std::io 对应物）

| 文件 | 内容 |
|------|------|
| `console.rs` | 终端核心：设备无关终端服务——`Console`（File 视图 + `insert_char`）、`InputHandler`（设备中断 handler）、`RawFile`（/dev/uartN 原始字节流）、`InputBuffer`、非泛型 `register(&'static dyn ByteChannel)` |
| `print.rs` | 输出路由 + 格式化宏：`OUT` 锁 + `filetable::resolve("/dev/console")` → 终端 File 写入（链接不存在回落 sbi 无锁直写）+ 宏 `print!`/`println!`/`tprint!`/`tprintln!` |
| `stdio.rs` | `Stdin`/`Stdout`（File 视图非阻塞 + 阻塞句柄方法 + `impl Read`/`Write`/`fmt::Write` 双视图） |

终端注册链路：每个终端 driver 实现 `trait ByteChannel`（hal/byte_channel.rs），probe 时经
`io::console::register(uart)` 注册——终端核心构造 `Console`（File，挂 /dev/consoleN）、`RawFile`
（原始字节流，挂 /dev/uartN）与设备中断 `InputHandler`（搬 FIFO → `Console::insert_char`）；
devfs 构建时 `/dev/console → 首个 consoleN` 符号链接表达 preferred（改链接即换系统控制台）。

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 查询 API 用查找动词，返回 `Option` | `registry::all()`、`resolve` |
| trait 描述能力而非动作 | `File`/`Read`/`Write` 是能力 trait，不是动作函数 |
| 错误码语义即行为 | `PermissionDenied` → 拒绝访问；`NotSupported` → 未实现操作默认 |
| 寄存器访问走固有方法 | `Stdin`/`Stdout` 固有方法与 `File`/`Read`/`Write` trait 方法并存（同名经固有优先/UFCS 区分） |

## 5. 生命周期与并发

- `Inode` 引导期 `Box::leak` 构建后不可变；`filetable` 全局 `RwLock`（将来随多任务改 per-process）。
- 复用器模型保证 file 域不反向依赖提供方；`/dev/log` 的环依赖已通过移除节点化解。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（VFS layer）。
