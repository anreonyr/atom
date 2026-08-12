# atom 系统架构图

> 本文件以 Mermaid 图呈现 atom 内核的整体架构。
> 代码事实以各模块契约文档 (`docs/src/*.md`) 为准。

---

## 1. 分层架构总览

```mermaid
graph TB
    subgraph User["👤 U-mode"]
        UPROG["用户程序 (ET_EXEC ELF)"]
        SHELL["内部 Shell (shell.rs)"]
    end

    subgraph Syscall["🔀 系统调用层 (runtime/envcall.rs)"]
        ECALL["Ecall 分发<br/>read/write/exit/spawn/wait/kill<br/>open/close/seek/map/unmap<br/>gettimeofday/fstat/readdir/create"]
    end

    subgraph VFS["📁 VFS + 服务集成 (file/)"]
        FILETABLE["filetable<br/>fd 表 + offset 管理"]
        INODE["Inode 树<br/>静态 children + 动态 Directory"]
        REGISTRY["registry<br/>File 注册表"]
        CONSOLE["io/console<br/>终端核心(Console/RawFile/InputHandler)"]
        BLOCKIO["io/block<br/>BlockFile + BlockIrqHandler"]
        FS["fs/<br/>极简 FS: superblock/inode/位图/目录"]
        PRINT["io/print<br/>OUT 锁 + println! 路由"]
        STDIO["io/stdio<br/>Stdin/Stdout"]
    end

    subgraph Schedule["⏱️ 调度层 (schedule/)"]
        SPAWN["spawn<br/>TaskBuilder::loader / TaskBuilder::new"]
        SCHEDULER["scheduler<br/>Round-Robin + CURRENT"]
        SLEEP["sleep<br/>时间阻塞 + 事件等待/通知"]
        WAIT["wait/kill/exit<br/>僵尸回收 + 退出码"]
    end

    subgraph Runtime["⚡ 运行时 (runtime/)"]
        TRAP["trap_handler<br/>中断/异常/ecall 分发"]
        CONTEXT["context<br/>TrapFrame 布局"]
        PANIC["panicking<br/>SBI 直写 + CSR dump + backtrace"]
    end

    subgraph Memory["🧠 内存管理 (memory/)"]
        PORTAL["portal<br/>#[global_allocator] → &dyn Allocator"]
        BUMP["bump (early)"]
        HYBRID["hybrid (Buddy + SegList)"]
        SPACE["AddressSpace<br/>Sv39 页表 + ASID"]
        FAULT["fault<br/>缺页处理"]
    end

    subgraph HAL["🔌 HAL 能力契约 (hal/)"]
        BYTECHAN["ByteChannel<br/>字节收发能力"]
        BLOCKDEV["BlockDevice<br/>块读写能力"]
        INTERRUPT["Interrupt<br/>InternalInterrupt/ExternalInterrupt/InterruptHandler"]
        CSR["csr<br/>S-mode CSR 封装"]
        RTC["rtc<br/>Realtime 契约"]
    end

    subgraph Driver["🔧 驱动层 (driver/)"]
        HUB["hub<br/>DTB 发现 → compatible 匹配 → deferred probe"]
        UART["uart/<br/>NS16550A / SiFive UART"]
        PLIC["controller/plic<br/>外部中断控制器"]
        CLINT["controller/clint<br/>定时器 + IPI"]
        VIRTIO["block/virtio_blk<br/>现代 virtio-mmio 块设备"]
    end

    subgraph Platform["🖥️ 平台层"]
        SBI["sbi<br/>SBI ecall 封装"]
        DTB["platform<br/>DTB 解析 + 全局配置"]
    end

    subgraph HW["🖥️ 硬件 (QEMU riscv64 virt)"]
        CPU["RISC-V Harts"]
        MMIO["UART / PLIC / CLINT / VirtIO"]
        MEM["DRAM"]
    end

    %% User → Syscall
    UPROG -->|"ecall (U→S)"| ECALL
    SHELL --> UPROG

    %% Syscall → VFS / Schedule / Memory
    ECALL --> FILETABLE
    ECALL --> SPAWN
    ECALL --> SPACE

    %% VFS internals
    FILETABLE --> INODE
    FILETABLE --> REGISTRY
    INODE --> CONSOLE
    INODE --> BLOCKIO
    INODE --> FS
    INODE --> STDIO
    FS --> BLOCKIO
    PRINT --> INODE

    %% VFS → HAL
    CONSOLE --> BYTECHAN
    BLOCKIO --> BLOCKDEV
    CONSOLE --> INTERRUPT
    BLOCKIO --> INTERRUPT

    %% Schedule → Memory
    SPAWN --> SPACE
    SCHEDULER --> CONTEXT

    %% Runtime → Schedule
    TRAP --> SCHEDULER

    %% Memory internals
    PORTAL --> BUMP
    PORTAL --> HYBRID
    SPACE --> FAULT

    %% Driver → HAL (implements)
    UART -.->|"impl"| BYTECHAN
    VIRTIO -.->|"impl"| BLOCKDEV
    PLIC -.->|"impl"| INTERRUPT
    CLINT -.->|"impl"| INTERRUPT

    %% Driver → Platform
    HUB --> DTB
    HUB --> UART
    HUB --> PLIC
    HUB --> CLINT
    HUB --> VIRTIO

    %% Platform → HW
    SBI --> CPU
    DTB --> MMIO
    DTB --> MEM

    %% Styling
    classDef user fill:#e1f5fe,stroke:#0288d1
    classDef syscall fill:#fff3e0,stroke:#f57c00
    classDef vfs fill:#e8f5e9,stroke:#388e3c
    classDef sched fill:#fce4ec,stroke:#c62828
    classDef runtime fill:#f3e5f5,stroke:#7b1fa2
    classDef memory fill:#fff8e1,stroke:#f9a825
    classDef hal fill:#e0f2f1,stroke:#00695c
    classDef driver fill:#e3f2fd,stroke:#1565c0
    classDef plat fill:#efebe9,stroke:#4e342e
    classDef hw fill:#eceff1,stroke:#37474f

    class UPROG,SHELL user
    class ECALL syscall
    class FILETABLE,INODE,REGISTRY,CONSOLE,BLOCKIO,FS,PRINT,STDIO vfs
    class SPAWN,SCHEDULER,SLEEP,WAIT sched
    class TRAP,CONTEXT,PANIC runtime
    class PORTAL,BUMP,HYBRID,SPACE,FAULT memory
    class BYTECHAN,BLOCKDEV,INTERRUPT,CSR,RTC hal
    class HUB,UART,PLIC,CLINT,VIRTIO driver
    class SBI,DTB plat
    class CPU,MMIO,MEM hw
```

---

## 2. 启动序列

```mermaid
sequenceDiagram
    participant QEMU as QEMU
    participant OSBI as OpenSBI (M-mode)
    participant KERNEL as atom Kernel
    participant DRAM as DRAM

    QEMU->>OSBI: 上电复位 (0x1000)
    OSBI->>OSBI: 初始化 M-mode 环境
    OSBI->>KERNEL: mret → _start (0x80200000, S-mode)

    rect rgb(255, 248, 225)
        Note over KERNEL,DRAM: Phase 0: early 初始化
        KERNEL->>KERNEL: _start asm: 设栈 → early()
        KERNEL->>KERNEL: allocator::init (bump → hybrid)
        KERNEL->>KERNEL: space::init (Sv39 根页表 + KERNEL_SPACE)
        KERNEL->>KERNEL: trap::init (stvec 就位)
    end

    rect rgb(227, 242, 253)
        Note over KERNEL,DRAM: Phase 1: 驱动探测
        KERNEL->>KERNEL: driver::init (hub DTB 发现)
        KERNEL->>KERNEL: deferred probe: PLIC → CLINT → UART → VirtIO
        KERNEL->>KERNEL: 终端注册 (io::console::register)
        KERNEL->>KERNEL: 块设备注册 (io::block::register)
    end

    rect rgb(232, 245, 233)
        Note over KERNEL,DRAM: Phase 2: VFS + Console + Log
        KERNEL->>KERNEL: 注册 stdin/stdout/null/zero
        KERNEL->>KERNEL: create_devfs (枚举 registry 建 /dev 树)
        KERNEL->>KERNEL: /dev/console → 首个 consoleN 符号链接
        KERNEL->>KERNEL: filetable::set_root + fd 0/1 预置
        KERNEL->>KERNEL: 极简 FS mount → /data 子树
        KERNEL->>KERNEL: log::init_timestamp
    end

    rect rgb(252, 228, 236)
        Note over KERNEL,DRAM: Phase 3: 中断使能
        KERNEL->>KERNEL: clock::tick::start (首次 10ms 定时中断)
        KERNEL->>KERNEL: sie::set(SEIE) + sie::set(STIE)
        KERNEL->>KERNEL: sstatus::set(SIE) ⚡ 中断开启
    end

    KERNEL->>KERNEL: main: spawn(Entry::Kernel(task_a))
    KERNEL->>KERNEL: WFI idle loop → 调度器驱动轮转
```

---

## 3. Trap 分发与系统调用路径

```mermaid
flowchart TD
    TRAP_ENTRY["trap_vector (naked asm)<br/>保存全部寄存器 → 调 trap_handler"] --> HANDLER["trap_handler (Rust)"]

    HANDLER -->|"scause.INTERRUPT"| INT{"中断类型?"}
    INT -->|"1: SSI"| SSI["csrc sip.SSIP<br/>scheduler() 立即重排"]
    INT -->|"5: STI"| STI["clock::tick::on_timer()<br/>重装定时器 + jiffies++<br/>软定时器分发 → scheduler()"]
    INT -->|"9: SEI"| SEI["get_external().claim()<br/>INTERRUPT_HANDLERS[source]<br/>→ complete() → scheduler()"]

    HANDLER -->|"synchronous"| SYNC{"scause.code?"}
    SYNC -->|"8: ecall from U"| ECALL_DISP["envcall::dispatch()"]

    ECALL_DISP --> ECALL_MATCH{"Ecall::from_number(a7)"}
    ECALL_MATCH -->|"63 read"| SYS_READ["sys_read(fd, buf, len)<br/>filetable::read → 阻塞 WaitRead park"]
    ECALL_MATCH -->|"64 write"| SYS_WRITE["sys_write(fd, buf, len)<br/>filetable::write"]
    ECALL_MATCH -->|"93 exit"| SYS_EXIT["sys_exit(code)<br/>标 Reap → Reschedule"]
    ECALL_MATCH -->|"1000 map"| SYS_MAP["sys_map(va, pages)<br/>匿名页分配"]
    ECALL_MATCH -->|"1001 unmap"| SYS_UNMAP["sys_unmap(va, pages)<br/>页回收"]
    ECALL_MATCH -->|"1002-1005"| SYS_FILE["open/close/seek/control<br/>filetable 操作"]
    ECALL_MATCH -->|"1006 gettimeofday"| SYS_TIME["gettimeofday<br/>墙钟 = RTC锚点 + elapsed"]
    ECALL_MATCH -->|"1007 fstat"| SYS_FSTAT["fstat(fd)<br/>元数据: 类型+大小"]
    ECALL_MATCH -->|"1008 readdir"| SYS_READDIR["readdir(fd)<br/>目录子节点名"]
    ECALL_MATCH -->|"1009 create"| SYS_CREATE["create(path)<br/>动态目录建文件"]
    ECALL_MATCH -->|"1010 spawn"| SYS_SPAWN["spawn(blob, len)<br/>拷ELF → loader装载 → 新任务"]
    ECALL_MATCH -->|"1011 wait"| SYS_WAIT["wait(pid)<br/>阻塞等子退出 → wait_result"]
    ECALL_MATCH -->|"1012 kill"| SYS_KILL["kill(pid)<br/>他杀 → -ECHILD"]
    ECALL_MATCH -->|"other"| SYS_NOSYS["-ENOSYS"]

    SYNC -->|"12/13/15: page fault"| PF["memory::fault::handle_page_fault()<br/>UMode terminate / 内核 panic"]
    SYNC -->|"other"| PANIC["log + panic<br/>UMode 任务 terminate"]

    SYS_READ --> RESULT{"DispatchResult"}
    SYS_WRITE --> RESULT
    SYS_EXIT --> RESULT
    SYS_SPAWN --> RESULT
    SYS_WAIT --> RESULT
    SYS_KILL --> RESULT

    RESULT -->|"Ret(v)"| RET["写回 a0<br/>sepc += 4<br/>sret 返回用户态"]
    RESULT -->|"Reschedule"| RESCHED["任务已离开运行态<br/>trap_handler 调 scheduler(frame)"]

    SSI --> SCHED["scheduler(frame)"]
    STI --> SCHED
    SEI --> SCHED
    RESCHED --> SCHED
    SCHED --> RESTORE["恢复下一任务寄存器<br/>sret"]

    style TRAP_ENTRY fill:#f3e5f5,stroke:#7b1fa2
    style HANDLER fill:#f3e5f5,stroke:#7b1fa2
    style ECALL_DISP fill:#fff3e0,stroke:#f57c00
    style SCHED fill:#fce4ec,stroke:#c62828
```
************
---

## 4. VFS 与设备模型

```mermaid
flowchart TB
    subgraph 注册阶段["🔧 注册阶段 (Phase 1 probe)"]
        direction TB
        UART_DRV["Uart16550Driver::probe"]
        VIRTIO_DRV["VirtioBlkDriver::probe"]
        UART_DRV -->|"Box::leak → &'static"| UART_INST["Uart16550 实例<br/>impl ByteChannel"]
        VIRTIO_DRV -->|"Box::leak → &'static"| VIRTIO_INST["VirtioBlk 实例<br/>impl BlockDevice"]
    end

    subgraph 终端核心["📟 终端核心 (file::io::console)"]
        direction TB
        CONSOLE_REG["console::register(uart)"]
        CONSOLE_REG --> CONSOLE_VIEW["Console<br/>File 视图 (回显+缓冲+唤醒)<br/>→ /dev/consoleN"]
        CONSOLE_REG --> RAWFILE_VIEW["RawFile<br/>原始字节流<br/>→ /dev/uartN"]
        CONSOLE_REG --> INPUT_HANDLER["InputHandler<br/>impl InterruptHandler<br/>搬 FIFO → insert_char"]
    end

    subgraph 块设备集成["💾 块设备集成 (file::io::block)"]
        direction TB
        BLOCK_REG["block::register(dev)"]
        BLOCK_REG --> BLOCKFILE_VIEW["BlockFile<br/>File 视图 (offset 随机访问)<br/>→ /dev/block0"]
        BLOCK_REG --> BLOCK_IRQ["BlockIrqHandler<br/>impl InterruptHandler<br/>ack + signal_event(Block)"]
    end

    subgraph VFS层["📁 VFS 层"]
        direction TB
        REG["registry::register(name, file)"]
        DEVFS["create_devfs()<br/>遍历 registry → 建 /dev 树"]
        MOUNT["fs::mount(blockdev)<br/>→ /data 子树"]
        ROOT["根 Inode"]
    end

    subgraph 运行时访问["🔄 运行时访问"]
        direction TB
        RESOLVE["filetable::resolve(path)<br/>零锁路径解析"]
        FD["fd 表 (RwLock)<br/>per-open offset<br/>read/write/seek/fstat/readdir"]
    end

    subgraph 输出路由["📤 println! 输出路由"]
        direction TB
        PRINT_MACRO["println! / tprintln!"]
        OUT_LOCK["OUT 锁 (SpinLock)"]
        RESOLVE_CONS["resolve('/dev/console')<br/>→ /dev/console → consoleN 链接"]
        SBI_FALLBACK["链接不存在?<br/>回落 sbi::write_fmt 无锁直写"]
    end

    %% 连线
    UART_INST --> CONSOLE_REG
    VIRTIO_INST --> BLOCK_REG

    CONSOLE_VIEW --> REG
    RAWFILE_VIEW --> REG
    BLOCKFILE_VIEW --> REG
    INPUT_HANDLER -->|"注册到"| INT_TABLE["INTERRUPT_HANDLERS 表"]
    BLOCK_IRQ -->|"注册到"| INT_TABLE

    REG --> DEVFS
    BLOCKFILE_VIEW --> MOUNT
    DEVFS --> ROOT
    MOUNT --> ROOT

    ROOT --> RESOLVE
    RESOLVE --> FD

    PRINT_MACRO --> OUT_LOCK
    OUT_LOCK --> RESOLVE_CONS
    RESOLVE_CONS --> FD
    RESOLVE_CONS --> SBI_FALLBACK

    %% /dev 子树内容
    subgraph DEV_TREE["/dev 子树"]
        direction LR
        DEV_NULL["null"]
        DEV_ZERO["zero"]
        DEV_STDIN["stdin → Stdin"]
        DEV_STDOUT["stdout → Stdout"]
        DEV_CONS0["console0 → Console"]
        DEV_UART0["uart0 → RawFile"]
        DEV_CONS["console → console0 (符号链接)"]
        DEV_BLOCK0["block0 → BlockFile"]
    end

    subgraph DATA_TREE["/data 子树 (极简 FS)"]
        direction LR
        DATA_MSG["msg.txt<br/>动态目录懒物化"]
        DATA_OTHER["..."]
    end

    ROOT --> DEV_TREE
    ROOT --> DATA_TREE

    style UART_DRV fill:#e3f2fd,stroke:#1565c0
    style VIRTIO_DRV fill:#e3f2fd,stroke:#1565c0
    style CONSOLE_REG fill:#e8f5e9,stroke:#388e3c
    style BLOCK_REG fill:#e8f5e9,stroke:#388e3c
    style REG fill:#c8e6c9,stroke:#2e7d32
    style RESOLVE fill:#fff3e0,stroke:#f57c00
    style PRINT_MACRO fill:#fce4ec,stroke:#c62828
```

---

## 5. 内存管理架构

```mermaid
flowchart TB
    subgraph 全局分配器["#[global_allocator]"]
        PORTAL["portal<br/>&dyn Allocator trait object"]
    end

    subgraph 分配器实现["分配器实现"]
        BUMP["bump<br/>启动早期线性分配<br/>(heap 初始化前)"]
        HYBRID["hybrid<br/>Buddy 物理帧 + SegList 小块"]
        BUDDY["frame (Buddy)<br/>2^n 页物理帧分配"]
        SEGLIST["block (SegList)<br/>固定大小块快速分配"]
    end

    subgraph 地址空间["AddressSpace"]
        SPACE["AddressSpace<br/>Sv39 根页表 + ASID"]
        TABLE["table<br/>三级页表 walk/map/unmap"]
        ENTRY["entry<br/>PTE + PteFlags"]
        ASID["asid<br/>16位 ASID 分配器<br/>(0 保留内核)"]
    end

    subgraph 映射关系["Sv39 地址映射"]
        IDENTITY["恒等映射 (VA==PA)<br/>内核低半区: DRAM + MMIO"]
        HIGH_HALF["高半区映射<br/>内核代码/数据"]
        USER_SPACE["用户空间<br/>U-mode 代码/堆/栈<br/>逐段 R|W|X flags"]
    end

    subgraph 缺页处理["缺页处理"]
        FAULT_HANDLER["handle_page_fault()<br/>UMode: 匿名页按需分配 / terminate<br/>内核: panic"]
    end

    %% 切换时序
    PORTAL -->|"Phase 0 early"| BUMP
    PORTAL -->|"Phase 0 heap 初始化后"| HYBRID
    HYBRID --> BUDDY
    HYBRID --> SEGLIST

    %% 空间关系
    SPACE --> TABLE
    TABLE --> ENTRY
    SPACE --> ASID

    %% 映射
    SPACE --> IDENTITY
    SPACE --> HIGH_HALF
    SPACE --> USER_SPACE

    %% 缺页
    FAULT_HANDLER --> SPACE

    %% 分配器供给空间
    BUDDY -->|"物理帧"| TABLE

    style PORTAL fill:#fff8e1,stroke:#f9a825
    style BUMP fill:#ffecb3,stroke:#ff8f00
    style HYBRID fill:#ffecb3,stroke:#ff8f00
    style SPACE fill:#e8eaf6,stroke:#3949ab
    style IDENTITY fill:#e8f5e9,stroke:#388e3c
    style HIGH_HALF fill:#e8f5e9,stroke:#388e3c
    style USER_SPACE fill:#e1f5fe,stroke:#0288d1
```

---

## 6. 任务模型与生命周期

```mermaid
stateDiagram-v2
    [*] --> Created: spawn(Entry, SpaceRef)

    state Created {
        [*] --> KernelTask: Entry.Kernel(fn)
        [*] --> UserTask: Entry.User(VirtAddr)
    }

    Created --> Ready: 加入就绪队列



	[*] --> Running: scheduler() 选中

    Running --> Ready: 时间片耗尽 (STI 抢占)
    Running --> Sleeping: sleep(dur) / wait_event() / wait(pid)
    Running --> Zombie: exit(code) / kill / 异常终止
    Running --> Ready: r#yield (SSI self-IPI)

    Sleeping --> Ready: 定时器到期 / signal_event() / 子退出
    Sleeping --> Zombie: kill

    Zombie --> [*]: 父 wait 回收 / 孤儿立即回收

    note right of Running
        S-mode: SPP=1 内核态执行
        U-mode: SPP=0 用户态执行
    end note

    note right of Zombie
        僵尸语义:
        - 父存活但从不 wait → 悬挂保留
        - 孤儿/已收尸 → 立即回收
        - 回收时释放页表树 (Box<AddressSpace>)
    end note
```

---

## 7. 模块依赖方向 (原子 vs 组合)

```mermaid
graph LR
    subgraph 原子层["⚛️ 零/浅依赖原子模块"]
        SBI_ATOM["sbi<br/>SBI ecall 封装"]
        HAL_ATOM["hal<br/>硬件能力契约 (纯 trait)"]
        LOCK_ATOM["lock<br/>Spin/Bare/Rw/Rel/OnceLock"]
        MACROS_ATOM["macros<br/>通用宏"]
        CONTEXT_ATOM["runtime/context<br/>TrapFrame 布局"]
        CSR_ATOM["hal/csr<br/>CSR 封装"]
        OPS_ATOM["file/ops<br/>File trait 契约"]
        BYTECHAN_ATOM["hal/byte_channel<br/>ByteChannel 契约"]
        BLOCKDEV_ATOM["hal/block<br/>BlockDevice 契约"]
    end

    subgraph 组合层["🔗 组合模块 (编排原子)"]
        direction TB
        INIT["init<br/>启动序列编排 (顶层)"]
        RUNTIME["runtime<br/>trap/envcall/panic"]
        SCHEDULE_COMBO["schedule<br/>调度/任务管理"]
        MEMORY_COMBO["memory<br/>分配器/页表/地址空间"]
        FILE_COMBO["file<br/>VFS/devfs/fs/io"]
        DRIVER_COMBO["driver<br/>hub/device/驱动实现"]
        CLOCK_COMBO["clock<br/>时间管理"]
        LOG_COMBO["log<br/>日志系统"]
        PLATFORM_COMBO["platform<br/>DTB 探测"]
    end

    %% 组合依赖原子 (单向)
    INIT --> HAL_ATOM
    INIT --> LOCK_ATOM
    RUNTIME --> CONTEXT_ATOM
    RUNTIME --> CSR_ATOM
    FILE_COMBO --> OPS_ATOM
    FILE_COMBO --> BYTECHAN_ATOM
    FILE_COMBO --> BLOCKDEV_ATOM
    DRIVER_COMBO --> HAL_ATOM
    DRIVER_COMBO --> BYTECHAN_ATOM
    DRIVER_COMBO --> BLOCKDEV_ATOM
    MEMORY_COMBO --> LOCK_ATOM
    SCHEDULE_COMBO --> CONTEXT_ATOM

    %% 组合间依赖 (无环)
    INIT --> RUNTIME
    INIT --> DRIVER_COMBO
    INIT --> FILE_COMBO
    INIT --> MEMORY_COMBO
    INIT --> CLOCK_COMBO
    INIT --> LOG_COMBO
    INIT --> PLATFORM_COMBO
    RUNTIME --> SCHEDULE_COMBO
    RUNTIME --> CLOCK_COMBO
    FILE_COMBO --> DRIVER_COMBO
    DRIVER_COMBO --> PLATFORM_COMBO
    DRIVER_COMBO --> MEMORY_COMBO
    SCHEDULE_COMBO --> MEMORY_COMBO
    SCHEDULE_COMBO --> CLOCK_COMBO

    style SBI_ATOM fill:#e0f2f1,stroke:#00695c
    style HAL_ATOM fill:#e0f2f1,stroke:#00695c
    style LOCK_ATOM fill:#e0f2f1,stroke:#00695c
    style MACROS_ATOM fill:#e0f2f1,stroke:#00695c
    style CONTEXT_ATOM fill:#e0f2f1,stroke:#00695c
    style CSR_ATOM fill:#e0f2f1,stroke:#00695c
    style OPS_ATOM fill:#e0f2f1,stroke:#00695c
    style BYTECHAN_ATOM fill:#e0f2f1,stroke:#00695c
    style BLOCKDEV_ATOM fill:#e0f2f1,stroke:#00695c

    style INIT fill:#ffcdd2,stroke:#b71c1c
    style RUNTIME fill:#f3e5f5,stroke:#7b1fa2
    style SCHEDULE_COMBO fill:#fce4ec,stroke:#c62828
    style MEMORY_COMBO fill:#fff8e1,stroke:#f9a825
    style FILE_COMBO fill:#e8f5e9,stroke:#388e3c
    style DRIVER_COMBO fill:#e3f2fd,stroke:#1565c0
    style CLOCK_COMBO fill:#fff3e0,stroke:#f57c00
    style LOG_COMBO fill:#e0f2f1,stroke:#00695c
    style PLATFORM_COMBO fill:#efebe9,stroke:#4e342e
```

---

## 图例说明

| 颜色 | 层级 | 职责 |
|------|------|------|
| 🟦 浅蓝 | U-mode | 用户程序 / 内部 Shell |
| 🟧 浅橙 | 系统调用 | Ecall 分发 (read/write/exit/spawn/wait...) |
| 🟩 浅绿 | VFS + 服务集成 | 文件抽象 / 终端核心 / 块设备 / 极简 FS / 输出路由 |
| 🟥 浅红 | 调度 | Round-Robin 调度器 / 任务生命周期 |
| 🟪 浅紫 | 运行时 | Trap 分发 / envcall / panic |
| 🟨 浅黄 | 内存管理 | Portal 分配器 / Sv39 页表 / 地址空间 |
| 🟩 青绿 | HAL | 硬件能力契约 (纯 trait, 零依赖) |
| 🟦 蓝 | 驱动 | DTB 发现 + 型号驱动 (UART/PLIC/CLINT/VirtIO) |
| ⬜ 灰 | 平台 | SBI / DTB / 硬件 |
