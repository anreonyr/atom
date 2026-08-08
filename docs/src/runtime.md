# runtime 模块 API 契约

> 本文件是 `src/runtime/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Trap dispatch / U-mode tasks & envcall」迁出。

## 1. 职责

内核运行时（CPU/启动侧）：trap 分发（naked asm → Rust handler）、U-mode ecall 分发（envcall）、
panic handler。`context.rs` 为原子支撑（零依赖，随 trap 就近归位）。

边界：

- **不做调度**：切任务由 `trap_handler` 调 `scheduler(frame)`（控制层）；语义标状态在 dispatch（语义层）。
- **不做定时器装载**：STI 重装由 `clock::tick::on_timer` 完成，这里只分发。
- **S-mode 自身 ecall（scause=9）不经本模块**：MEDELEG bit9=0，留 M-mode OpenSBI。

## 2. 引导流程

`runtime::trap::init()` — stvec 先就位，driver 探测期间异常可进 trap_handler（而非重入 `_start`）；
之后所有 trap 都走这里。

## 3. 公共 API 面

### 3.1 Trap dispatch

```
trap_vector (naked asm)
  └─ trap_handler (Rust)
       ├── scause.INTERRUPT: scause.code()
       │    1 (SSI) → csrc sip.SSIP + scheduler()（r#yield 的 self-IPI 立即重排）
       │    5 (STI) → clock::tick::on_timer()（重装 + jiffies++ + 软定时器分发）→ scheduler::scheduler()
       │    9 (SEI) → get_external().claim() → INTERRUPT_HANDLERS[source] → complete()
       └── synchronous:
             8 (ecall from U-mode) → envcall::dispatch()（Ret：写回 a0、sepc+=4；Reschedule：exit 已标 Reap / read 已置 WaitRead park → scheduler 切下一任务）
             12/13/15 (page fault) → memory::fault::handle_page_fault()（未处理：UMode terminate / 内核 panic）
             others → log + panic（UMode 任务 terminate）
```

### 3.2 数据

- `context.rs` — `TrapFrame` 布局 + `FRAME_OFF_*` 偏移常量（原子；trap/scheduler 共享底层）。
- `INTERRUPT_HANDLERS` — `SpinLock<Vec<Option<&'static dyn InterruptHandler>>>` 按 IRQ 号索引，O(1) 查找。
  handler 归属：契约在 `hal::interrupt`，实现归 `file::io` 集成层（`InputHandler`/`BlockIrqHandler` 与
  register 同置，见 [driver.md 边界](driver.md)），driver 不实现 `InterruptHandler`。

### 3.3 envcall（U-mode ecall 分发，scause=8）

- `enum Ecall` — 变体即调用号，`from_number` 解码；match 转发到独立 `sys_*` 函数（read/write/map/unmap/exit）；
  未知号一律 `-ENOSYS`。a7=调用号、a0..a5=参数。
- `DispatchResult` 两臂：`Ret(v)` → 写回 a0 + `sepc += 4` 跳过 ecall；`Reschedule` → 当前任务已离开运行态
  （exit 标 Reap / read 置 WaitRead park），trap_handler 调 `scheduler(frame)` 切下一任务。
- 返回值写回 a0（负数 = -errno，`errno_of(FileError)` 统一映射）。
- 调用号：read=63 / write=64 / exit=93（Linux riscv64）；教学自定义号段（≥1000）：
  map=1000 / unmap=1001（堆匿名分配/释放，非 Linux mmap）、open=1002 / close=1003 /
  seek=1004 / control=1005、gettimeofday=1006 / fstat=1007 / readdir=1008、
  create=1009 / spawn=1010 / wait=1011 / kill=1012。
- gettimeofday 墙钟语义：boot 锚点 = RTC epoch 秒 + 同时刻 mtime 刻度（`envcall::init_wall_clock`，
  main 在 `init::run` 后调一次）；之后单调 elapsed 叠加——RTC 只读一次避免每次 syscall 读 MMIO。
  无 RTC（锚点未设）→ -ENODEV。tv 为用户区 2×u64（sec, usec），tz 忽略。
- read 阻塞：缓冲空 → `mark_input_wait`（WaitRead + resume_sepc=0 保持 ecall 地址）+ `Reschedule`——
  任务直接 park 进睡眠队列（无用户态忙转），字符到达 `wake_input_waiters` 唤醒后 sret 到 ecall 重放分发（幂等），缓冲已非空读到返回。
- wait 阻塞：目标存活 → `schedule::wait_sys` 置 `Pending::Wait(pid)` + resume_sepc=0 +
  `Reschedule`——任务 park 进睡眠队列；子退出（scheduler Reap）或被 kill（kill.rs）
  唤醒后 sret 重放 ecall，重入 `wait_sys` 读 `wait_result` 返回退出码（被杀 → -ECHILD）。
  SMode 就地 `schedule::wait`（wfi 原地恢复）不受影响——scheduler Reap 与 kill 的唤醒
  恢复点按 `TaskKind` 区分（UMode→0 重放 / SMode→`resume_after_wait` 原地）。
- spawn/kill：`spawn(blob, len)` 拷出用户内存 ELF → `TaskBuilder::loader` 装载进新空间
  子任务运行（返回子任务 id，父可 wait 收尸）；`kill(pid)` 他杀（0 / -ESRCH / -EPERM）。
- fd 语义：fd 0/1 经 VFS 全局表预置（fd 0=/dev/stdin 挂 Stdin、fd 1=/dev/stdout 挂 Stdout），read/write 走 `filetable`。

### 3.4 U-mode 任务

`spawn(entry: Entry, space: Option<Box<AddressSpace>>)` 是唯一任务创建入口（`Entry` 定义于 `schedule::spawn`）：

- `Entry::Kernel(fn())` — 内核任务：S-mode 运行，同步异常 → panic。
- `Entry::User(VirtAddr)` — 真 U 任务：U-mode 运行（初始帧 sstatus 不置 SPP，sret 进 U），异常 → terminate_current；
  入口须为已映射 U|R|X 的代码页（映射由调用方负责，demos 的 `map_user_code` 为参考实现）。

UMode 任务机制：

- 任务栈映射追加 `PteFlags::U`；trap_vector 入口置 `sstatus.SUM`（S-mode 写 U 页任务栈/读帧必需；
  csrsi 立即数是掩码语义不能表达 bit18，用 csrrw 借 t1 中转）。恢复段 `csrw sstatus` 后仍访问帧 →
  spawn 初始帧 sstatus 也带 SUM（喂首次 dispatch）。

### 3.5 panicking

- 内核 panic handler：`mprintln!`（SBI 输出，绕开全部锁）→ CSR dump + frame-pointer backtrace + SBI `system_reset`。
  改名 `panicking` 避免 `crate::panic` 与 `core::panic` 冲突。

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 错误码语义即行为 | `-ENOSYS`（未知号）、`-errno`（统一映射 `errno_of`） |
| 查询 API 用查找动词 | `Ecall::from_number -> Option<Ecall>` |

## 5. 生命周期与并发

- 中断**非嵌套**：`SIE` 进入时清、`sret` 恢复。
- `INTERRUPT_HANDLERS` 为 `SpinLock` 索引表（中断上下文安全）。

## 6. 变更记录

- 2026-08-08：`INTERRUPT_HANDLERS` 补 handler 归属说明——契约在 hal、实现归 file/io 集成层（指针见 [driver.md](driver.md)）。
- 2026-08-08：M5——新增 spawn(1010)/wait(1011)/kill(1012)（进程创建 syscall，无 fork/exec 语义，
  task-model 定夺）；wait 阻塞走 re-dispatch（`Pending::Wait` + resume_sepc=0 重放 ecall 读
  `wait_result`），scheduler Reap 与 kill 唤醒恢复点按 TaskKind 区分（UMode→0 / SMode→原地）。
- 2026-08-08：M2/M3——新增 gettimeofday(1006)/fstat(1007)/readdir(1008)；墙钟 boot 锚点 + `init_wall_clock`。
- 2026-08-08：从 CLAUDE.md 迁出（Trap dispatch / U-mode tasks & envcall）。
