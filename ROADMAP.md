# ROADMAP

> 评估主线：**能否满足程序（U-mode 用户程序）的需求**。内核子系统是否完备，不以
> "自己有多少组件"为准，而以**一个真实程序能否加载、运行、干活、退出**为准绳。
> 每项完成的验收 = 一条 QEMU 里可见的 U 程序行为。

<!--toc:start-->
- [ROADMAP](#roadmap)
  - [已完成](#已完成)
  - [下一步](#下一步)
    - [主线 M — 程序需求链](#主线-m--程序需求链)
    - [并行线 P — 不阻塞程序主线](#并行线-p--不阻塞程序主线)
  - [原则](#原则)
<!--toc:end-->

## 已完成

- [x] **启动序列** — `_start` asm 设置栈 → `early()` → `main` → 多阶段初始化
- [x] **门户分配器 (Portal)** — `#[global_allocator]` 经 `&dyn Allocator` 在 bump → hybrid 间切换
- [x] **锁家族** — SpinLock / BareLock / RwLock / RelLock / OnceLock（中断感知、可重入）
- [x] **CSR 封装** — sstatus/sie/stvec/scause/sepc/stval/satp 读写
- [x] **UART 驱动** — NS16550A + SiFive UART，hub/device/driver 模型，多实例注册表
- [x] **中断基础设施** — CLINT 定时器、PLIC 外部中断、SSI、非嵌套 trap 分发
- [x] **陷阱分发** — trap_vector(naked asm) + trap_handler：SSI/STI/SEI + 缺页 + 同步异常处置
- [x] **日志系统** — 五级日志 + 模块过滤 + 时间戳
- [x] **HAL trait 层** — Driver / InternalInterrupt / ExternalInterrupt / InterruptHandler
- [x] **DTB 设备发现** — 解析 FDT，hub 按 compatible 匹配 → probe（deferred 重试）
- [x] **MMU (Sv39)** — identity 内核空间 + 高半区映射 + 缺页处理
- [x] **物理帧分配器** — Buddy frame 分配器（任务栈、页表帧）
- [x] **S-mode 运行** — OpenSBI mret 后 S-mode，M-mode 操作经 SBI ecall 委托
- [x] **Round-Robin 调度器** — 就绪/睡眠/僵尸队列 + 定时器抢占
- [x] **任务管理** — spawn/wait/kill/yield、per-task 独立地址空间、sleep 阻塞唤醒、僵尸回收
- [x] **任务栈守护页** — TASK_STACK_BASE 窗口 + 栈底守护页拦截溢出 + 递归溢出专用路径
- [x] **地址空间所有权** — Task 持有 Box/Arc，Zombie 时释放页表树（无泄漏）
- [x] **VFS / devfs** — File trait + Inode 树 + fd 表 + /dev/stdin、/dev/stdout、/dev/null、/dev/zero、/dev/consoleN
- [x] **输出通道分层** — device 设备选择层 + print 带锁通道（OUT）+ mprint 无锁 SBI 直写
- [x] **日志模块化** — log/ 八子模块、LogMessage owned 统一、console/ring 级别分离（Linux printk 语义）
- [x] **时间管理** — Clock 契约 + 10ms tick + 软定时器 + sleep(Duration) 亚秒精度 + 忙等 delay
- [x] **ASID 独立分配** — 每任务独立 ASID，TLB 局部刷新
- [x] **ecall 系统调用框架** — enum Ecall 分发：read(63)/write(64)/exit(93) + map/unmap + open/close/seek/control；fd 经 VFS filetable；read 阻塞 park 无忙转
- [x] **U-mode 用户态进程** — 真 U-mode 执行、用户栈/堆布局、map/unmap 匿名页、TaskKind::UMode
- [x] **排查路径工具** — 边界断言、panic 上下文增强、QEMU gdbstub 流程、最小复现开关（DEMO_*）
- [x] **lockdep 最小版** — 四锁持有者溯源 + 单 hart 重入/死锁检测

## 下一步

### 主线 M — 程序需求链

按依赖顺序推进：**M1 → M2 → M3 → M4 → M5**。每一步的验收都是 QEMU 里一条
可见的 U 程序行为（当前缺 ELF loader，U 程序只能跑 `demos.rs` 的内嵌汇编——
程序需求链的第一个缺口）。

- [ ] **M1. ELF loader** — 程序运行的第一步（当前最大瓶颈：真实程序跑不起来）
  - 新模块 `loader/`：ELF64 解析（magic/ehdr/phdr 校验）+ 段装载
  - 装载语义：`PT_LOAD` → 代码 `R|X`、数据 `R|W`、`.bss` 零页（`memsz > filesz` 填零）
  - 落地 **`TaskBuilder::loader(blob)`** 静态工厂（不动 `Entry`；task-model 方案 A 的
    loader 形态），复用 `map_user_code` 的 U 页映射路径（逐段 flags）
  - **验收**：自写无 libc 的 RISC-V 静态 ELF（`_start` 直接 syscall），
    `spawn` 跑起来输出 + `exit(0)`，`wait` 收回退出码

- [ ] **M2. 时间 syscall** — 程序感知时间的第二痛（内核 clock 完整但 U 程序拿不到）
  - `gettimeofday`（自定义号 **1006**）：返回 epoch 秒 + 微秒，复用 `clock` 内核 API
    （`ticks_to_usecs` + RTC）
  - **验收**：U 程序循环计时，打印两时点差值非零

- [ ] **M3. 文件元数据 syscall** — 程序操作文件的支撑
  - `fstat`（**1007**）：文件大小/类型；`readdir`（**1008**）：目录列举（devfs 目录树已可遍历）
  - **验收**：U 程序 `open("/dev/console0")` 后 `fstat` 得到 ByteDevice 类型，`readdir("/dev")` 列出节点名

- [ ] **M4. 块设备 + 极简文件系统** — 程序"能干活"的分水岭（能读写持久数据）
  - `virtio-blk` 驱动：driver/ 新角色目录，参照 `io::uart` 三联动模板
    （块设备 → 设备选择表 + `/dev/sda` + 块 `File` 实现）；依赖 PLIC（deferred 机制已备）
  - **自制极简 FS**：inode 表 + 数据区 + 目录（`File` trait 的 offset 参数已为块设备设计，天然对接）
  - **验收**：U 程序写入一个数据文件，重启后再读回一致（持久性）

- [ ] **M5. 进程创建 syscall** — 程序系统完整（可派生子进程）
  - `spawn`/`fork` syscall（复用 schedule 的 spawn + wait/kill，syscall 化）
  - `exec_current`（task-model 落点：换 space + 改 sepc）
  - **验收**：U 程序派生子任务，子任务独立输出，父 `wait` 收回退出码

### 并行线 P — 不阻塞程序主线

可插空推进，优先级低于主线：

- [ ] **Superpage** — `map_region` 支持 2MB（L1）/ 1GB（L2）大页（性能优化）
- [ ] **多核启动** — 多 hart 唤醒、per-hart 栈与 CURRENT、per-hart 中断（独立大工程）
- [ ] **调试改进** — `/dev/log` 丢消息检测（`log_seq_range` 预留 API）、
      `set_console_level` 调用点、lockdep 完整版（锁序图 + 中断上下文染色）

## 原则

- **一次只做一件事** — 完成并 commit 后再开始下一个
- **先跑起来再优化** — 能用比完美重要
- **测试驱动** — 每个新特性都要能在 QEMU 里看到效果（主线以 **U 程序行为**为验收，
  而非内核自身日志）
