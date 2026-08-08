# loader 模块 API 契约

## 1. 职责

把无 libc 的 RISC-V 静态 ELF（ET_EXEC）解析 + 逐段装载进新地址空间，生产
`(AddressSpace, entry)`——`TaskBuilder::loader(blob)` 消费它 spawn 出 U-mode
任务。这是程序需求链的起点（ROADMAP M1）：真实程序经它才能跑起来。

边界：

- **不做** symbol/重定位解析（只接受 ET_EXEC 静态链接，无动态重定位）。
- **不做** syscall/进程管理（那属于 `runtime/envcall.rs` / `schedule/`）。
- **不做** 任务栈映射（`spawn` 统一映射固定窗口 `TASK_STACK_BASE`）。
- **不做** .bss 段后加载分配器（堆由 `map`/`unmap` ecall 管理，见 memory.md）。

## 2. 引导流程

```
用户程序（ET_EXEC）──scripts/build-user.sh──► user/user.elf（提交入库）
        │ include_bytes!（mod.rs::user_program）
        ▼
   blob: &[u8] ──loader::load──► 解析(elf.rs) → 建空间(from_kernel) → 逐段映射
        │
        ▼
   (Box<AddressSpace>, entry_va) ──spawn.rs::TaskBuilder::loader──► spawn()
        ▼
   U-mode 任务：write/exit syscall → 父任务 wait 收回退出码
```

调用时机：`TaskBuilder::loader(blob)?.spawn()`（demo 见 `demos.rs::demo_loader`）。

## 3. 公共 API 面

### 3.1 函数

| API | 签名 | 语义与契约 |
|-----|------|-----------|
| `load` | `pub fn load(blob: &[u8]) -> Result<(Box<AddressSpace>, VirtAddr), LoadError>` | 解析 + 建空间 + 逐段装载，返回 `(space, entry)`。**不 panic**：失败返回 `LoadError`（见下），调用方决定降级（日志 + 不 spawn） |
| `user_program` | `pub fn user_program() -> &'static [u8]` | 内嵌的验收探针 ELF 字节（`include_bytes!` 编译期嵌入） |

### 3.2 数据

`LoadError`（`loader::load` 返回的错误）：

| 变体 | 行为语义 |
|------|---------|
| `Elf(ElfError)` | 格式非法/不支持——调用方日志 + 不 spawn |
| `Map(MapError)` | 页表操作失败（`from_kernel`/`space.map`：页表帧耗尽 / 段重叠 `AlreadyMapped`） |
| `OutOfMemory` | 数据物理帧耗尽 |

`ElfError`（`loader::elf` 内部错误，变体名即行为）：

| 变体 | 触发 |
|------|------|
| `Truncated(&'static str)` | 长度不足 / 程序头表越界 / PT_LOAD 文件区间越界 |
| `BadFormat(&'static str)` | magic / class / endianness / version 不符 |
| `Unsupported(&'static str)` | 非 ET_EXEC（ET_DYN/ET_REL）、非 RISC-V、PN_XNUM |
| `NotUserAddress(&'static str)` | e_entry / PT_LOAD vaddr 不在用户半区 |
| `InvalidSegment(&'static str)` | memsz < filesz / 无 PT_LOAD / entry 不在段内 / 地址溢出 |

`Elf64<'a>`（解析结果，`loader::elf`）：`entry()` 入口 VA、`phnum()` 程序头数、
`phdr(i) -> Option<Segment>`（非 PT_LOAD 或越界返回 `None`）。

`Segment`（PT_LOAD 视图）：`flags`(PF_X/W/R)、`offset`、`vaddr`、`filesz`、`memsz`。

### 3.3 常量

| 名称 | 值 | 语义 |
|------|-----|------|
| `PT_LOAD` | 1 | 可装载段类型号（其余跳过） |
| `PF_X` / `PF_W` / `PF_R` | 1 / 2 / 4 | 段权限位（loader 恒置 R） |
| 装载基址 | `0x1_0000` | 用户程序 link.ld 基址（与 `demos.rs USER_CODE_VA` 一致） |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 读写对称 | 解析结果只读访问：`entry()`/`phdr()` 无 `get_` 前缀 |
| 查询 API 用查找动词，返回 `Option` | `phdr(i) -> Option<Segment>`；调用方决定跳过非 PT_LOAD |
| 错误消息带 `&'static str` 上下文 | `ElfError` 各变体 payload 为出错点定位（"phdr table"/"PT_LOAD vaddr"…） |
| 错误码语义即行为 | `LoadError`/`ElfError` 变体名直接对应调用方处置 |
| 不用缩写 | 全称 `Segment`/`Segment` 而非 `ph`；`Elf64` 而非 `eh` |

## 5. 生命周期与并发

- **无全局静态**：loader 无状态，每次 `load` 新建空间，无锁。
- **blob 为 `'static` 内嵌**（`user_program`），装载只读拷贝进物理帧——blob 不需要
  存活过 `load`。
- **`unsafe` 边界**：清零/拷贝直接写物理帧 PA（DRAM 恒等映射区可写）；区间由
  `parse` 校验（文件区间在 blob 内、段不溢出），`SAFETY:` 注释标注。
- **帧回收已知限制**：`AddressSpace::Drop` 只归还页表帧、不归还叶子数据帧——
  装载的数据页随空间泄漏（与 `map_user_code` 现状一致，教学内核可接受；
  M5 进程创建前需解决）。
- **ET_EXEC 前置**：只接受静态链接（非 PIE），故段绝对地址 = 装载 VMA，无需
  基址重定位。PIE（ET_DYN）暂拒绝（`Unsupported`）。

## 6. 变更记录

- 2026-08-08：M1 落地——`loader/` 模块（elf.rs 原子解析 + load.rs 段装载）、
  `TaskBuilder::loader` 静态工厂、`user/` 用户程序子 crate + 构建脚本、`DEMO_LOADER`
  验收 demo。QEMU 闭环：`hello from elf` + `[ELF] wait = Some(0)`。
