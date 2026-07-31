# ROADMAP

<!--toc:start-->
- [ROADMAP](#roadmap)
  - [已完成](#已完成)
  - [进行中](#进行中)
  - [下一步](#下一步)
    - [内核基础设施](#内核基础设施)
    - [内存管理](#内存管理)
    - [核心 OS 能力](#核心-os-能力)
    - [外设与存储](#外设与存储)
  - [原则](#原则)
<!--toc:end-->

## 已完成

- [x] **启动序列** — `_start` asm 设置栈 → `early()` → `main` → 多阶段初始化
- [x] **门户分配器 (Portal)** — `#[global_allocator]` 通过 `&dyn Allocator` trait object 在启动阶段切换后端：`bump` (早期) → `hybrid` (运行时)
- [x] **Bump Allocator** — 早期引导用，支持 `Box`/`Vec`/`String` 在 MMU 和堆初始化前使用
- [x] **Hybrid Allocator** — 组合分配器，运行时全局堆后端
- [x] **SpinLock / BareLock / RwLock / RelLock / OnceLock** — 完整的锁原语家族，中断感知
- [x] **CSR 封装** — sstatus/sie/stvec/scause/sepc/stval/satp 读写
- [x] **UART 输出** — NS16550A，115200 baud，`fmt::Write` 实现
- [x] **UART 中断输入** — 按键 → PLIC 中断 → 回显
- [x] **CLINT 定时器** — `time` CSR 读取 + SBI `set_timer`，定时器中断驱动调度
- [x] **PLIC 外部中断** — 优先级/使能/claim/complete
- [x] **陷阱分发** — 完整 trap_vector(naked asm) + trap_handler：SSI/STI/SEI + 缺页异常
- [x] **日志系统** — 五级日志 + 编译期过滤 + ANSI 颜色 + CLINT 时间戳
- [x] **HAL trait 层** — `Driver` / `InternalInterrupt` / `ExternalInterrupt` / `InterruptHandler` / `Mmio`
- [x] **设备发现 (DTB)** — 引导期解析 Flattened Device Tree，提取 compatible/base/size/interrupt
- [x] **设备注册中心 (hub)** — 按 `(TypeId, name)` 注册/查询具体类型实例（无 fat pointer），取代旧的 trait object 注册表
- [x] **驱动实例管理** — `Box::leak` 创建 `'static` 实例，`hub::register` 统一管理，支持同类型多设备
- [x] **MMU (Sv39)** — 恒等映射内核空间，identity-map DRAM + MMIO 设备，缺页异常处理
- [x] **物理帧分配器** — 位图页分配器（6 MiB DRAM），MMU 初始化使用
- [x] **S-mode 运行** — 内核直接在 S-mode 运行（OpenSBI mret 后），通过 SBI ecall 委托 M-mode 操作
- [x] **Round-Robin 调度器** — `VecDeque<*mut TrapFrame>` 就绪队列，定时器中断触发切换
- [x] **任务创建 (spawn)** — 栈分配 + TrapFrame 构造 + 入队列
- [x] **Panic Handler** — CSR dump、frame-pointer 回溯、专用 PANIC_UART 通路绕过所有锁、SBI 关机
- [x] **集成测试框架** — QEMU 启动 → 检查 UART 输出 → 判断测试通过

## 进行中

- [ ] **Hub 重写**（第 1 步 ✅） — 去掉 fat pointer，只存具体类型（已完成）
- [ ] **设备文件抽象 (devfs)** — 将 hub 中的具体设备实例暴露为统一文件接口

## 下一步

### 内核基础设施

- [ ] **SBI 扩展** — IPI、Hart State Management 等扩展调用
- [ ] **多核启动** — 多 hart 唤醒、per-hart 栈
- [ ] **时间管理** — 高精度定时器、sleep/msleep 抽象

### 核心 OS 能力

- [ ] **用户态进程** — U-mode 执行、ecall 系统调用、用户态缺页处理
- [ ] **系统调用接口** — 基本的 syscall 框架（open/read/write/ioctl…）
- [ ] **进程调度增强** — 优先级、时间片、阻塞/唤醒
- [ ] **地址空间隔离** — 每个进程独立的页表

### 外设与存储

- [ ] **virtio-blk** — 块设备驱动，读取磁盘
- [ ] **简单文件系统** — FAT32 或自制极简 FS
- [ ] **devfs 挂载** — `/dev/uart0`、`/dev/plic0`、`/dev/clint0` 等设备文件

### 设备文件抽象 (devfs)

- [ ] **设计文件 trait** — `Read`/`Write`/`Ioctl`/`Seek` 等
- [ ] **devfs 实现** — open/close 包装 `hub::get`
- [ ] **驱动接入** — 各驱动实现文件 trait
- [ ] **print 迁移** — 从 hub 直接取 UART 改为通过 fd 输出

### Trap 细化

- [ ] 把散落的错误处理集中

## 原则

- **一次只做一件事** — 完成并 commit 后再开始下一个
- **先跑起来再优化** — 能用比完美重要
- **测试驱动** — 每个新特性都要能在 QEMU 里看到效果
