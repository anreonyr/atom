# ROADMAP

<!--toc:start-->
- [ROADMAP](#roadmap)
  - [已完成](#已完成)
  - [下一步](#下一步)
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
- [x] **任务管理** — spawn/spawn_with、per-task 独立地址空间、sleep 阻塞唤醒、exit/terminate、僵尸栈回收
- [x] **任务栈守护页** — TASK_STACK_BASE 窗口 + 栈底守护页拦截溢出 + 递归溢出专用路径
- [x] **地址空间所有权** — Task 持有 Box，Zombie 时释放页表树（无泄漏）
- [x] **VFS / devfs** — File trait + Inode 树 + fd 表 + /dev/consoleN、/dev/log、/dev/null、/dev/zero
- [x] **错误处理重构** — init::Error / DriverError / Result 传播（历史清单见 `docs/archive/TODO.md`）
- [x] **排查路径工具** — 边界断言、panic 上下文增强、QEMU gdbstub 流程、最小复现开关（见 `docs/debugging.md`）
- [x] **上下文切换健壮性** — `sleep(Duration)` 时长 API、TaskKind::SMode/UMode 语义、TrapFrame 抽离 `context.rs`（offset_of! 编译期锁定 asm 偏移）、trap 入口 sscratch 交换（守护页检查不污染任务寄存器）、wfi 提前返回状态复位、中断注册重复检测（记录见 `TODO.md`）

## 下一步

### 用户态与系统调用
- [ ] **U-mode 用户态进程** — U 模式执行、用户栈/堆布局
- [ ] **ecall 系统调用框架** — open/read/write/ioctl 分发（当前同步异常 UMode 任务直接 terminate）
- [ ] **进程调度增强** — 优先级、时间片、fork/exit 完整语义

### 内存演进
- [ ] **Superpage** — `map_region` 支持 2MB（L1）/ 1GB（L2）大页
- [ ] **ASID 独立分配** — 每任务独立 ASID，TLB 局部刷新（`switch_space` 已支持 ASID 参数）

### 多核与性能
- [ ] **多核启动** — 多 hart 唤醒、per-hart 栈与 CURRENT、per-hart 中断
- [ ] **时间管理** — 高精度定时器抽象（`sleep(Duration)` 已落地：任意时长/亚秒精度，换算依赖 timebase 频率）

### 外设与存储
- [ ] **virtio-blk** — 块设备驱动
- [ ] **简单文件系统** — FAT32 或自制极简 FS（当前 devfs 仅内存节点）

### 已知限制
- `TASK_STACK_BASE` 依赖内核不映射 L2[3] 且 DRAM < 1GiB（boot 期有断言）
- 递归压栈溢出由专用路径处置（User terminate / kernel panic）

## 原则

- **一次只做一件事** — 完成并 commit 后再开始下一个
- **先跑起来再优化** — 能用比完美重要
- **测试驱动** — 每个新特性都要能在 QEMU 里看到效果
