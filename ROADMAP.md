# ROADMAP

## 已完成

- [x] **启动序列** — `_start` asm 设置栈 → `main` → 三阶段初始化
- [x] **Bump Allocator** — 64 KiB 堆，支持 `Box`/`Vec`/`String`
- [x] **SpinLock** — 单原子布尔自旋锁，保护共享资源
- [x] **CSR 宏** — `csr_read!` / `csr_write!` / `csr_set!` 内联汇编
- [x] **UART 输出** — NS16550A，115200 baud，`fmt::Write` 实现
- [x] **设备注册表** — `TypeId` 驱动的 trait object 注册/获取/替换
- [x] **CLINT 定时器** — mtime/mtimecmp，定时器中断 + 软件中断
- [x] **PLIC 中断控制器** — 初始化/优先级/claim/complete
- [x] **中断分发** — MTI/MSI/MEI 三类中断的路由与处理
- [x] **日志系统** — 五级日志 + 编译期/运行时双层过滤 + ANSI 颜色 + 时间戳
- [x] **HAL trait 层** — `Timer` / `IrqHandler` / `InterruptController` 抽象

## 下一步（选一个方向）

### 基础增强
- [ ] **中断驱动 UART 输入** — 收到按键 → PLIC 中断 → 回显字符
- [ ] **更好的 Panic Handler** — 打印寄存器 dump、调用栈回溯
- [ ] **集成测试框架** — QEMU 启动 → 检查 UART 输出 → 判断测试通过

### 内存管理
- [ ] **物理内存分配器** — 替换 bump allocator，支持 free（buddy 或 slab）
- [ ] **页表 / 虚拟内存** — Sv39 页表，内核恒等映射

### 核心 OS 能力
- [ ] **S-mode 切换** — 从 M-mode 进入 S-mode，设置 mret 目标
- [ ] **用户态进程** — U-mode 执行、ecall 系统调用、时钟中断抢占
- [ ] **简单调度器** — round-robin，任务队列，上下文切换

### 外设与存储
- [ ] **virtio-blk** — 块设备驱动，读取磁盘
- [ ] **简单文件系统** — FAT32 或自制极简 FS

## 原则

- **一次只做一件事** — 完成并 commit 后再开始下一个
- **先跑起来再优化** — 能用比完美重要
- **测试驱动** — 每个新特性都要能在 QEMU 里看到效果
