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

- [x] **M1. ELF loader** — 程序运行的第一步（当前最大瓶颈：真实程序跑不起来）
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
  - **实现方式 = 复用「终端核心提炼」的 Console 接缝模板**（hal 能力契约 →
    非泛型 register → 设备 → File 接缝 → 中断 handler 归设备）：
    - **能力契约 `hal::BlockDevice`**：`block_size`/`block_count`/`read_block`/
      `write_block`（object-safe `&dyn`，仿 `hal::ByteChannel`）
    - **驱动 `driver/block/virtio_block.rs`**：`impl BlockDevice + Driver`，probe
      map_mmio → virtqueue 初始化（共享内存 DMA）→ `dev.set_instance` →
      `block::register`；依赖 PLIC（deferred 机制已备）
    - **服务集成 `file/io/block.rs`**：`BlockFile` 实现 `File`——**read/write 用
      offset**（与 Console 忽略 offset 的流式不同；块设备随机访问正是 `File`
      offset 参数的主战场，filetable 把 `OpenFile::offset` 传入）；非泛型
      `register(&'static dyn BlockDevice)` → `/dev/block0`
  - **自制极简 FS**：块之上建 Inode 树（inode 表 + 数据区 + 目录），每个目录/
    数据文件是一个 `&dyn File`；**VFS 需支持多子树/挂载**（devfs 是枚举 registry
    建 /dev 树，真实 FS 需解析块设备建第二棵子树或新挂载点）
  - **中断 = 完成通知**（与 Console 的「数据到达」语义不同）：`BlockIrqHandler`
    实现 `InterruptHandler`（handler 归设备），virtio 完成中断 → 标记请求完成 +
    唤醒 `block_wait()` 等待者
  - **验收**：U 程序写入一个数据文件，重启后再读回一致（持久性）

- [ ] **M5. 进程创建 syscall** — 程序系统完整（可派生子进程）
  - `spawn`/`fork` syscall（复用 schedule 的 spawn + wait/kill，syscall 化）
  - `exec_current`（task-model 落点：换 space + 改 sepc）
  - **验收**：U 程序派生子任务，子任务独立输出，父 `wait` 收回退出码

### 并行线 P — 不阻塞程序主线

可插空推进，优先级低于主线：

- [x] **终端核心提炼** — 把 `io::uart` 里绑死在 `Uart` 泛型上的终端服务（缓冲/
      回显/唤醒/`File` 适配/注册）提炼为**设备无关的终端核心**：能力 trait 用
      trait object 擦除（非泛型三视图 + 非泛型注册），新终端设备只需实现
      "字节收发 + 中断"能力即可复用；同时产出通用 **设备 → `File`** 接缝，
      M4 块设备接入与未来任何终端设备复用（应在加第二种终端前落地）
      （已完成 2026-08-08：`hal::ByteChannel` 能力契约 + `file::io::console`
      终端核心 `Console`/`RawFile`/`InputHandler` + 非泛型 `register`；
      `io::device` 设备表删除，preferred 由 `/dev/console → consoleN` 链接表达）
  - **落地形态 · 终端核心节点**：`consoleN` = **终端核心节点**（`InputBuffer`
    + 回显/唤醒 + 终端状态，持有 `&dyn` 字节收发能力引用），**非软链接别名**；
    `uartN` = ByteDevice 硬件节点（字节收发 + 中断，无终端语义），两者经内核
    对象引用关联——`/dev/consoleN`（终端服务 File）与 `/dev/uartN`（原始字节
    流 File）是**不同语义**。消除 `io::device` 独立设备表，preferred 由
    `/dev/console → consoleN` 符号链接表达（改链接即换系统控制台）
  - **落地形态 · 锁职责分离**：`OUT`（print 层）只管输出串行化；file 层锁只
    管数据结构（registry），devfs 树不可变后路径解析**无锁**——println! 路径
    为 OUT → 只读解析链接 → 终端核心写入，比当前 `OUT → DEVICES` 少一层
  - **落地形态 · print/log 特权级解耦**：`println!`/`mprintln!` 去特权化——
    print 纯格式化（单一出口），console 层封装输出目标（正常 → 解析
    `/dev/console` 链接 → `File` 写入；boot 早期 / panic → 无锁 SBI 直写），
    print/log 不再感知 S/M 特权模式
  - **落地形态 · 多终端骨架**：多 UART（driver 多实例）→ 每 `consoleN` 独立
    InputBuffer/回显/唤醒 + 各自 shell 任务 = **多用户最小骨架**（多人各占一
    终端独立读写）；tty 会话语义（控制终端/进程组）为后续独立增量
- [ ] **Superpage** — `map_region` 支持 2MB（L1）/ 1GB（L2）大页（性能优化）
- [ ] **多核启动** — 多 hart 唤醒、per-hart 栈与 CURRENT、per-hart 中断（独立大工程）
- [ ] **调试改进** — `/dev/log` 丢消息检测（`log_seq_range` 预留 API）、
      `set_console_level` 调用点、lockdep 完整版（锁序图 + 中断上下文染色）

## 原则

- **一次只做一件事** — 完成并 commit 后再开始下一个
- **先跑起来再优化** — 能用比完美重要
- **测试驱动** — 每个新特性都要能在 QEMU 里看到效果（主线以 **U 程序行为**为验收，
  而非内核自身日志）
