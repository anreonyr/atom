# atom API 设计评审

> 本文档以 **项目自身声明的约定** 为评审准绳（不引入外部 OS 风格），
> 以 **模块为单位** 逐个评审：职责 → 完整公共 API 面 → 五维评估
> （原语 / 正交性 / 命名 / 自洽性 / 模块组织）→ 问题清单与建议。
> 末尾给出跨模块综合与优先级。纯评审，不含代码修改。

## 0. 评审基线（CLAUDE.md 自声明约定）

| # | 约定 | 内容 |
|---|------|------|
| 1 | 读写对称 | 读方法=字段名（无 `get_` 前缀）；写方法=`set_`+字段名；动词型写入并入 `set_` 范式（`bind`→`set_driver`） |
| 2 | 不用缩写 | 用体现原语的完整词（`instance` 而非 `drvdata`） |
| 3 | trait 描述能力 | trait 方法=自描述数据+生命周期回调（`Driver::name/compatibles/probe`）；拒绝伪抽象（行为不同的 init 不强行 trait 化） |
| 4 | 查询用查找动词 | `find`/`get` 表达查找，返回 `Option`，调用方决定降级 |
| 5 | 错误码语义即行为 | 变体名直接对应处理行为（`Deferred`→重试，`Stuck`→终止），错误消息带 `&'static str` 上下文 |
| 6 | 同名方法防歧义 | `File` 与 `Mmio` 均有 read/write 时，内部寄存器访问走固有方法 `read_reg`/`write_reg` |
| 7 | 注释约定 | `//` 文件头块、`///` pub、`//` 私有；中文描述+英文关键词（`# Safety`/`# Errors`/`SAFETY:`） |
| 8 | 锁选择 | SpinLock 默认中断安全 / BareLock 仅任务态（unsafe lock）/ RwLock 读重 / RelLock 可重入 / OnceLock 写一次读多次 / LazyLock 惰性 |
| 9 | 实例生命周期 | `Box::leak` 得 `&'static`；实例挂 `Device`；`bus::find` 查询；无 `pub static mut` |

## 1. 模块清单与边界

14 个评审单元，按 src/ 结构划分，职责以模块头注释自我声明 + 调用方用法为准：

`platform`（config/dtb/header/qemu_virt）· `sbi` · `memory`（addr/entry/table/space/fault + allocator{portal,bump,hybrid,block,frame,page}）· `hal`（csr/mmio/interrupt/cpu）· `driver`（traits/device/bus + serial{uart16550,sifive_uart} + controller{plic,clint}）· `filesystem`（traits/inode/filetable + dev{log,null,zero}）· `scheduler` · `trap` · `lock`（spin/bare/rw/reentrant/once/lazy/trap/log）· `log` · `print` · `panic` · `init` · `main`

---

## 2. platform

**职责**：引导早期从 DTB 探测全局硬件参数（DRAM/timebase/hart 数），提供只读全局配置；零分配解析 FDT。

**公共 API 面**

| API | 签名 | 状态 |
|-----|------|------|
| `Platform` | `{dram_base, dram_size, timebase_frequency: u64, stack_reserve, hart_count} `（全 pub 字段） | ✅ |
| `init` | `unsafe fn init(dtb_ptr: usize)` | ✅ |
| `get` | `fn get() -> &'static Platform`（未 init 则 panic） | ✅ |
| `report_probe_error` | `fn ()` | ✅ |
| `take_saved_dtb` | `unsafe fn () -> Option<usize>`（pub(crate)） | ✅ |
| `Dtb` | `new(ptr)` / `find_compatible` / `find_type` / `find_path` / `walk()` | find_* 为 dead_code 预留 |
| `Node` | `name` / `property_reg` / `property_u32` / `property_string` / `property_raw` | ✅（property_raw 预留） |
| `qemu_virt` | `DRAM_BASE/SIZE` `UART_BASE/SIZE/INTERRUPT` `CLINT_BASE/SIZE` `PLIC_BASE/SIZE` `TIMEBASE_FREQ` | ✅ 回退默认 |
| `PAGE_SIZE` | `const usize` | ✅ |

**五维评估**

- 原语：✅ `Platform` 只读配置语义清晰；DTB 零分配解析是好原语。
- 命名：`config.rs` 模块内类型名为 `Platform` 而模块名为 `config`——类型路径 `platform::config::Platform`，常用访问是 `platform::get()`，类型名与模块名错位。
- 自洽：⚠️ **`static mut PLATFORM` 绕过 OnceLock**（config.rs:43,96-100）——"引导期写一次、此后只读高频访问"正是项目锁表规定的 OnceLock 用途（panic UART、log 时钟源同款用法），现用手写 `static mut` + unsafe `addr_of!` 读，安全论证散落；`SAVED_DTB`（config.rs:46）同理。`PROBE_ERROR` 用 BareLock ✅（自述纯任务上下文）。
- 正交：⚠️ config 承担"全局配置 + DTB 指针缓存 + 探测错误缓存"三职责（config.rs:43-50），靠 `take_saved_dtb`/`report_probe_error` 两个 pub(crate) 接口穿针。
- 模块组织：⚠️ `Dtb::find_*` 查询面 dead_code，而 `driver/device.rs:128-154` 自己 walk 解析设备——同一 DTB 存在两套解析入口，重复且分裂。

**问题与建议**

1. `PLATFORM`/`SAVED_DTB` 改 `OnceLock`，消除 unsafe 读路径（OnceLock 无堆依赖，allocator 前可用）。
2. `Dtb::find_*` 与 `device::probe` 的解析职责收敛：要么 device 用 find_*，要么删掉 find_*。
3. 类型名 `Platform` 与模块名 `config` 二选一对齐（如 `config::Config` + `platform::get()`）。

**结论**：★★☆ 职责清晰，但 static mut 绕过既定原语、DTB 双解析入口待收敛。

---

## 3. sbi

**职责**：S-mode 经 `ecall` 委托 M-mode（OpenSBI）的原子服务面。

**公共 API 面**

| API                                           | 签名                                                                | 状态               |
| --------------------------------------------- | ----------------------------------------------------------------- | ---------------- |
| `putchar`                                     | `fn (ch: u8)`（legacy console，panic 安全）                            | ✅                |
| `set_timer`                                   | `fn (stime_value: u64)`（绝对时间，v2.0 高低 32 位）                        | ✅                |
| `system_reset`                                | `fn (reset_type: u32, reset_reason: u32) -> !`                    | ✅                |
| `RESET_TYPE_SHUTDOWN/COLD_REBOOT/WARM_REBOOT` | `const u32`                                                       | 后两者 dead_code 预留 |
| `ecall`                                       | `unsafe fn (ext_id, func_id, args: [usize; 6]) -> (usize, usize)` | 私有               |

**五维评估**：✅ 原语级干净：三个功能函数 + 一个底层 unsafe 通道，参数类型明确，`-> !` 表达不返回。叶模块零依赖（仅 core::arch）。`set_timer` 与 RISC-V 术语一致。轻微：reset_type 无新类型封装（`RESET_TYPE_*` 裸 u32），可接受。

**结论**：★★★ 无问题，保留原样。

---

## 4. memory

**职责**：地址类型（addr）、Sv39 PTE（entry）、三级页表（table）、地址空间与 Region（space）、缺页解析（fault）、分配器体系（allocator）。

### 4.1 addr / entry

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `VirtAddr` | `new(usize) -> Option<Self>`（规范检查） | dead_code（与 from_raw 并存） |
| | `from_raw(usize)`（符号扩展） / `vpn(level)` / `offset()` / `as_usize()` / `is_user()` | ✅ |
| | `page_align()` / `is_kernel()` | dead_code 预留 |
| `PhysAddr` | `from_raw` / `is_aligned()` / `as_usize()` | ✅ |
| | `page_align()` | dead_code 预留 |
| `PteFlags` | bitflags：V/R/W/X/U/G/A/D | ✅ |
| `PageTableEntry` | `empty` / `new` / `new_branch` / `is_valid` / `is_leaf` / `is_branch` / `is_user` / `ppn` / `paddr` / `flags` / `as_u64` / `set` / `set_flags` / `clear` | new_branch/is_user/as_u64 等 dead |

**评估**：强类型 VA/PA 封装 ✅ 良好。⚠️ `VirtAddr::new`（规范检查）dead 而 `from_raw`（恒合法）在用——两个构造器并存，安全语义模糊（from_raw 从不失败意味着 "new" 的检查被绕过）。⚠️ `is_kernel` dead 而 `is_user` 在用——不对称。⚠️ `PageTableEntry` 整 impl 标 `#[allow(dead_code)]`（entry.rs:39），`new_branch` 未被用（`walk_mut` 用 `set(ppn, V)` 手动构造 branch）——API 冗余。

### 4.2 table / space / fault

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `MapError` | `OutOfMemory/AlreadyMapped/NotAligned/NotMapped/NoRegion` | **pub(crate) 但泄漏进 pub API** |
| `PageTable` | `allocate/deallocate/walk_ref/walk_mut/map/unmap/clean`（全 pub(crate)） | ✅ |
| `AddressSpace` | `new` / `from_kernel` / `map` / `map_region` / `unmap` / `protect` / `page_fault` / `translate` / `root_page -> u64` / `region_add` / `region_find` / `share_kernel` | unmap/protect/share_kernel/region_remove 为 dead_code 预留 |
| `Region`/`RegionKind` | `{start,end,flags,kind}`；Anonymous/Reserved | Reserved dead_code |
| `kernel_space` | `() -> RelLockGuard<Option<AddressSpace>>` | ✅ |
| `space::init` | `unsafe fn () -> Result<(), MapError>` | ✅ |
| `PageFault` | `{addr: VirtAddr, pc, kind}` / `FaultKind{Instruction,Load,Store}` / `capture()` | ✅ |
| `handle_page_fault` | `fn (&PageFault, &AddressSpace) -> bool` | ✅ |
| `switch_space` | `unsafe fn (root_page_number: usize, asid: usize)` | ✅（asid 恒 0） |
| `flush_tlb` | `unsafe fn ()` | ✅ |
| `map_device` | `unsafe fn (base: usize, size: usize, allocator: &dyn Allocator)` | ⚠️ base 用裸 usize |

**评估**

- 原语：✅ Region+Anonymous 懒分配、shared_l2 释放保护是扎实的原语设计。
- 自洽：⚠️ **`MapError` 是 `pub(crate)`，但 `AddressSpace::map/map_region/page_fault/region_add` 等 pub 方法返回它**（space.rs:141 等）——pub API 暴露不可见错误类型，外部无法 match 变体只能 Debug；`init::InitError::Memory(table::MapError)`（init.rs:23）同样引用。当前 bin crate 无外部消费者所以能编译，但暴露面不完整。
- 类型连贯：⚠️ `AddressSpace::root_page() -> u64`（space.rs:269）而 `switch_space(root_page_number: usize)`（memory/mod.rs:37）——页号类型 u64/usize 混用。
- 模块组织：⚠️ **`map_device(base: usize)` 丢弃强类型**（memory/mod.rs:66）——`Device.base` 是 `PhysAddr`（device.rs:47），驱动调用时 `dev.base.as_usize()` 转换（uart16550.rs:210），map_device 内部又转回 `VirtAddr::from_raw`/`PhysAddr::from_raw`（memory/mod.rs:84-86）——强类型在边界往返丢失。
- ⚠️ `map` 与 `map_region` 双方法并存（space.rs:141,166）：仅"是否自动取整"之差，命名区分度低。
- ⚠️ 4 个 mmap 系方法（unmap/protect/share_kernel/region_remove）dead_code 悬空（空间预留，无调用方），与 Region 管理 API 形成"半成品面"。

### 4.3 allocator 体系

**公共 API**

| 后端 | API |
|------|-----|
| portal | `PortalAllocator`（`#[global_allocator]` PORTAL_ALLOCATOR）+ `switch(&'static dyn Allocator)` |
| bump | `init()` / `allocator()` / `frontier()` / `boundary()` |
| hybrid | `init()` / `allocator()`（≤4KiB→block，>4KiB→frame） |
| block | `init()` / `allocator()`（segregated free list，per-hart 数组 OnceLock） |
| frame | `init()` / `allocator()`（Buddy，SpinLock<Option<FrameInner>>） |
| page | `allocator()`（PageAllocator ZST，强约束 layout=页，委托 frame） |
| 顶层 | `memory::allocator::init()`（编排 bump→hybrid） |

**评估**

- 命名：✅ 五个后端同名 `allocator()` 对称；`init()` 一致。
- 原语：✅ hybrid 按大小路由是简单清晰的原语；page 层把"页对齐布局"约束收口。
- 自洽：⚠️ **分配器间以 `bump::frontier()/boundary()` 隐式耦合**（frame.rs:110-121 依赖 bump 边界）——初始化顺序是脆弱隐式契约（注释已详述，但无类型/编译期保证）；block 内又依赖 frame（refill 取页），形成 bump→hybrid→{block→frame} 的初始化链。
- ⚠️ `block::allocator()` 在未初始化时 `expect("block allocator not initialized")`（block.rs:243-247）——panic 而非 Result，与"查询 API 返回 Option 由调用方降级"约定略偏（但属引导期不变式，可接受）。

**memory 问题与建议**

1. `MapError` 提升为 `pub`，或把 AddressSpace 的 pub 方法错误改为模块级 `SpaceError` 包装。
2. `map_device` 改收 `PhysAddr`（与 Device.base 对齐），内部不再往返转换。
3. `root_page()` 与 `switch_space` 统一页号类型（如 `PhysPage` 或统一 usize）。
4. 收敛 `VirtAddr::new`/`from_raw`、`new_branch`、`is_kernel` 等 dead API（二选一或删除）。
5. mmap 系 4 方法：要么给出真实调用方（syscall 后端），要么删除并移除 `#[allow(dead_code)]`。

**结论**：★★☆ 强类型地址与 Region 设计好；错误可见性、类型往返、分配器隐式耦合待修。

---

## 5. hal

**职责**：硬件抽象 trait 层（Mmio/Interrupt）+ S-mode CSR 类型安全封装（csr）+ hart 标识（cpu）。

**公共 API**

| 子模块 | API |
|--------|-----|
| csr::sstatus | `Sstatus` bitflags{SIE,SPIE} + `SPP: usize` 裸 const；`read/write`(dead)/`set/clear` |
| csr::sie | `Sie` bitflags{SSIE,STIE,SEIE}；`read/write`(dead)/`set`/`clear`(dead) |
| csr::stvec | `write(addr)` |
| csr::scause | `Scause`{INTERRUPT} + `CODE_MASK` + `is_interrupt/code`；`read`/`write`(dead) |
| csr::sepc | `read` / `write`(dead) |
| csr::stval | `read` |
| csr::satp | `MODE_SV39` / `make(mode,asid,ppn)` / `read/write` / `mode(val)` / `ppn(val)` |
| mmio | `Mmio` trait：`type T: Copy` + `base() -> *mut u8` + `unsafe read/write` 默认实现 |
| interrupt | `InternalInterrupt`{frequency/read/next/handle_timer 默认/trigger_soft(dead)}；`ExternalInterrupt`{init/ enable/ claim/ complete}；`InterruptHandler`{interrupt_number/handle_interrupt/enable_interrupt}；`register_internal/external` + `get_internal/external` |
| cpu | `HartId`（new/as_usize）+ `unsafe hart_id()`（TODO 多核） |

**五维评估**

- 命名：✅ csr 子模块 API 高度统一（read/write/set/clear + bitflags），dead_code 标注"CSR 抽象完整性"合理。
- 原语：⚠️ **`Mmio` trait 只有 UART 两个实现者**（uart16550.rs:120、sifive_uart.rs:101）——单实现者伪抽象风险；且其 `read/write` 与 `File::read/write` 同名（约定 6 已承认，UART 用 `read_reg/write_reg` 固有方法规避——规避本身说明 trait 命名侵入性）。
- 自洽：⚠️ **`ExternalInterrupt::init -> Result<(), &'static str>` 是全局唯一裸字符串错误**（interrupt.rs:53），违反约定 5"错误码语义即行为"；PLIC probe 中 `plic.init().map_err(DriverError::Init)?`（plic.rs:93）恰好把它转进 DriverError，说明错误语义本可统一。⚠️ **`sstatus::SPP` 是裸 `usize` const**（csr.rs:38）而非 `Sstatus` bitflags 成员——scheduler.rs:552-553 用 `SPIE.bits() | SPP` 拼 usize，类型割裂（SPP 是字段不是位，但 API 面应统一封装）。
- 正交：⚠️ `InternalInterrupt::handle_timer` 默认实现（interrupt.rs:16-18"以 frequency 为间隔重装"）把 **CLINT 特定行为放进通用 trait 默认实现**，而 clint.rs:52-55 的覆写与原样重复——默认实现应文档化或移除。
- 模块组织：✅ 三个中断 trait（Internal/External/Handler）职责正交；注册面 OnceLock 单实例（多控制器扩展受限，见架构评审）。

**问题与建议**

1. 定义 `InterruptError` 枚举或复用 `DriverError`，替换 `ExternalInterrupt::init` 裸字符串。
2. `SPP` 收进 sstatus 封装（如 `Sstatus::SPP` 成员或提供 `sstatus::set_spp()`）。
3. `Mmio` 若无第二实现者：降级为 UART 固有 `read_reg/write_reg` 并删除 trait；保留则改名 `read_reg/write_reg` 消除同名歧义成本。
4. 删除 `InternalInterrupt::handle_timer` 默认实现，由 CLINT 显式提供。

**结论**：★★☆ csr 层一致性最好；中断 trait 错误类型、SPP 封装、Mmio 伪抽象待修。

---

## 6. driver

**职责**：bus/device/Driver 模型 + 驱动实现（串口双型号、PLIC/CLINT）。

**公共 API**

| 子模块 | API |
|--------|-----|
| traits | `DriverError{Deferred, Init(&'static str), MapFailed(&'static str), Stuck}`；`Driver` trait{`name()`(dead)/`compatibles()`/`probe(&Device)`} |
| device | `DeviceState{Unbound,Deferred,Bound,Unsupported}`；`Device{compatible: &'static str, base: PhysAddr, size: usize, interrupt: Option<u32>, status: SpinLock}`；`new`(pub(crate))/`state`/`driver`(dead)/`set_state`/`set_driver`/`set_instance`/`instance::<T>`；`probe() -> Vec<Device>` |
| bus | `Bus`；`bus() -> &'static Bus`；`init() -> Result<(), DriverError>`；`find::<T>() -> Option<&'static T>`；`find_all::<T>() -> Vec<&'static T>`(dead) |
| serial | `SerialDevice{file: &'static dyn File, writer: &'static dyn fmt::Write}`；`DRIVERS`；`register(file, writer)`/`all()`/`console() -> Option<&'static dyn fmt::Write>`；`Uart16550`（new/read_reg/write_reg/write_byte(pub(crate))/init_hw + Mmio/File/fmt::Write/InterruptHandler impl）；`SifiveUart` 同构 |
| controller | `DRIVERS`；`Plic`（new/set_priority + ExternalInterrupt）；`Clint`（new + InternalInterrupt + Clock impl） |

**五维评估**

- 命名：✅ 读写对称典范——`set_state`/`state`、`set_driver`/`driver`、`set_instance`/`instance`（device.rs:77-110，约定 1 执行到位）。⚠️ `bus::bus()` 模块函数与模块同名（bus.rs:113），调用形如 `bus::bus().devices...` 别扭。
- 原语：✅ `DriverError::Deferred/Stuck` 语义即行为（bus.rs 重试循环直接消费）；实例 downcast（`instance::<T>` Any 查询）是贴合的 Linux dev_set_drvdata 对应物。⚠️ `Driver::name` dead_code（traits.rs:36）——元数据 API 无消费者。
- 自洽：⚠️ **两套查询机制并存**：`bus::find_all::<T>`（按实例类型，dead_code，bus.rs:127-135）vs `serial::all` 注册表（跨型号+file/writer 双视图，在用）。devfs 走注册表而非 find_all——注册表职责合理（双视图需求），但 find_all 成为无主 dead API。
- 正交/模块组织：⚠️ **serial 模块依赖 `filesystem::traits::File`**（serial/mod.rs:11）——driver 层依赖 VFS 层类型（见跨模块依赖环）；⚠️ **双 UART 驱动重复代码**：`File::write` 与 `fmt::Write::write_str` 的 `\r\n` 逐字节逻辑在两文件中各复制两份（uart16550.rs:128-168、sifive_uart.rs:109-151）——DRY 违反；⚠️ **`File::read` 是轮询忙等**（uart16550.rs:135-137 `while LSR&1==0 { spin_loop() }`）——任务上下文无 sleep 集成，VFS 读会烧 CPU。
- 细节：`Uart16550::new(base: usize)` 与 `Device.base: PhysAddr` 类型不一致（probe 里 `.as_usize()` 丢弃）；`irq` 默认 `unwrap_or(10)`/`unwrap_or(4)`（uart16550.rs:207、sifive_uart.rs:194）——**型号相关的硬编码默认值藏在 probe 里**，与 DTB interrupt 缺失时的语义应更显式。

**问题与建议**

1. 提取 `write_byte` + `\r\n` 转换到公共实现（trait 默认方法或共享辅助），消除双驱动重复。
2. `bus::find_all` 与 serial 注册表：让 devfs 基于 find_all（若可行）或删除 find_all；至少文档化两套机制的分工。
3. `File::read` 轮询改为"无数据返回 WouldBlock/0"或接入 sleep 阻塞（为 U-mode console 输入铺路）。
4. 删除 `Driver::name` 或给真实消费者（日志/调试）。

**结论**：★★★ bus/device/Driver 模型与读写对称执行是亮点；重复代码、双查询面、轮询读待收敛。

---

## 7. filesystem

**职责**：File 能力 trait + 静态 Inode 树 + 全局 fd 表 + devfs 工厂。

**公共 API**

| 子模块 | API |
|--------|-----|
| traits | `FileError{NotFound,NotSupported,InvalidFd,PermissionDenied(dead),IoError,Eof,InvalidArg,NotDirectory(dead)}`；`Result<T>`；`File` trait{`read(offset,buf)`/`write(offset,buf)`/`seek(pos,current)`(dead)/`control(cmd,arg)`(dead)，全默认 NotSupported}；`SeekFrom{Start,Current,End}`(dead)；`OpenFlags{READ,WRITE,RDWR(dead),is_readable/is_writable(dead)}` |
| inode | `InodeType{Directory,ByteDevice}`；`Inode{name,inode_type,file: Option<&'static dyn File>,children}`；`InodeBuilder{new,with_file,with_child,build}`；`lookup(root,path)` |
| filetable | `OpenFile{inode,offset,flags(dead)}`；`FileTable::new`；`set_root`；`open(path,flags)->Result<usize>`/`close`/`read(fd,buf)`/`write(fd,buf)`/`seek`(dead)/`control`(dead) |
| dev | `create_devfs() -> &'static Inode`；`LOG`/`NULL`/`ZERO` static + `LogDev`/`NullDev`/`ZeroDev` impl File |

**五维评估**

- 原语：✅ **`File` trait 是能力化范本**：四方法全带默认实现（NotSupported 降级），实现者按能力覆盖（traits.rs:49-87）；offset 由 VFS 维护、调用时传入（约定 3 的典型执行）。✅ `OpenFlags` 位集合 + 方法自描述。
- 命名：✅ `SeekFrom`/`FileError` 语义清晰；`create_devfs` 动词型工厂。
- 自洽：⚠️ 大量预留 dead API 集中在 traits/filetable：`seek`（filetable.rs:133）、`control`（filetable.rs:148）、`SeekFrom` 全变体、`PermissionDenied/NotDirectory`、`RDWR/is_*`——VFS 偏移与 ioctl 语义已实现但未接通调用方，形成"实现完成、无消费者"的悬空面。
- 正交：⚠️ `InodeType` 仅 `Directory/ByteDevice` 两类（inode.rs:20-25）——常规文件/符号链接无法表达，VFS 无法承载真实存储（架构评审已列）；`Inode` 引导期构建后不可变（inode.rs:4-5）——无动态目录（mkdir/rm 不可能）。
- 模块组织：⚠️ **devfs 依赖 `driver::serial::all`**（dev/mod.rs:32）——filesystem 依赖 driver；反向 UART impl File（见跨模块综合依赖环）。
- 细节：`filetable` 是全局函数式 API（`open/read/write` 自由函数 + 全局 `FILE_TABLE`）——per-process 化时整体移入 Task（注释已自述 filetable.rs:6,46）；`File::read/write` 返回值 `Result<usize>` 与 log_read 的裸 `usize` 不一致（dev/log.rs 透传 OK）。

**问题与建议**

1. 接通或收敛悬空的 seek/control/错误码预留（决定"VFS 偏移 API"是否本阶段启用）。
2. `InodeType` 增加 Regular（为真实 FS 铺路）或明确 devfs-only 边界。
3. 依赖环处理见跨模块综合（把"文件能力出口"从驱动 impl 剥离到注册表契约面）。

**结论**：★★★ File trait 能力化与 Inode 静态树是亮点；悬空预留与 InodeType 表达力待扩展。

---

## 8. scheduler

**职责**：任务状态机（就绪/阻塞/僵尸）+ round-robin 抢占 + sleep/exit/terminate + 地址空间切换与栈守护。

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `Task` | `{id, state, kind, frame: *mut TrapFrame, space: Option<Box<AddressSpace>>, stack, stack_size, wake_tick, resume_sepc}`（pub(crate)） | ✅ |
| `TaskState` | `Ready/Blocked/Zombie` | ✅ |
| `TaskKind` | `Kernel/User` | ⚠️ User 名不副实 |
| `spawn` | `fn (entry: fn())` | ✅ |
| `spawn_with` | `fn (entry: fn(), space: Box<AddressSpace>)` | ✅ |
| `sleep` | `fn (ticks: u64)` | ⚠️ 单位问题 |
| `exit` | `fn (code: i32) -> !` | ✅ |
| `terminate_current` | `fn (frame: *mut TrapFrame) -> usize`（pub(crate)） | ✅ |
| `current_space` | `() -> Option<*const AddressSpace>` | ✅（裸指针逃逸锁） |
| `current_is_user` | `() -> bool`（pub(crate)） | ⚠️ 语义是"TaskKind==User"非真 U-mode |
| `scheduler` | `fn (frame: *mut TrapFrame) -> usize` | ✅ |
| 常量 | `TASK_STACK_BASE=0xC0000000`、`STACK_SIZE=16384` | ✅ |

**五维评估**

- 原语：✅ Task 状态机 + 三队列（TASK_QUEUE/SLEEP_LIST/ZOMBIE_LIST）+ CURRENT 单点，结构清晰；`resume_sepc`+wfi 唤醒技巧自洽（scheduler.rs:279-324）。
- 命名：⚠️ **`sleep(ticks)` 参数单位三方不一致**（scheduler.rs:422 注释"ticks 个定时器周期" vs 实现 `ticks.saturating_mul(freq)`= 秒 vs 文档"sleep(ticks)"）——`sleep(1)` 实际睡 1 秒（10MHz timebase）；命名即语义违反。
- 自洽：⚠️ `TaskKind::User` 只是"异常处置策略标签"——spawn_impl 置 `sstatus = SPIE | SPP`（scheduler.rs:552-553，SPP=Supervisor），所有任务跑 S-mode；"用户任务"语义与实现不符（架构评审已列，此处为 API 命名证据）。
- 类型：⚠️ `frame: *mut TrapFrame` 裸指针 + `space: Box` 所有权混用（设计使然，注释详尽）；`current_space()` 返回裸指针以逃逸锁生命周期——调用方（fault）需自行保证安全。
- 模块组织：⚠️ scheduler 依赖 `trap::TrapFrame` 布局（scheduler.rs:30）且被 trap 反向依赖（见跨模块综合）。

**问题与建议**

1. `sleep` 改 `sleep_seconds(secs)` 或改实现为真实 tick 数；同步修正 wake_tick 单位文档。
2. `TaskKind::User` 在 U-mode 落地前改 `TaskKind::Unprivileged`（S-mode 语义）或文档明示"仿真用户语义"。
3. `spawn(entry: fn())` 参数类型限制"只能跑内核内函数指针"——ELF 加载前先文档化。

**结论**：★★☆ 状态机完整扎实；sleep 单位与 User 名实是命名层面最需修的缺口。

---

## 9. trap

**职责**：trap_vector（naked asm 保存/恢复帧）+ trap_handler（中断/异常分发）+ 中断处理器注册表。

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `TrapFrame` | repr(C)，32 通用寄存器 + sepc + sstatus（34 字段） | ✅ |
| `trap_vector` | `unsafe extern "C" fn()`（naked） | ✅ |
| `trap_handler` | `fn (frame: *mut TrapFrame) -> usize`（返回下一帧） | ✅ |
| `trap_stack_corrupt` | `fn () -> usize` | ✅ |
| `init` | `unsafe fn ()`（写 stvec） | ✅ |
| `register_interrupt_handler` | `fn (&'static dyn InterruptHandler)` | ⚠️ 无注销/共享 IRQ |

**五维评估**

- 原语：✅ naked asm 保存/恢复 + `a0` 传帧 + 返回值递帧，路径干净；栈守护页检测（sp 越界→trap_stack_corrupt）是扎实的健壮性设计。
- 命名：✅ trap_vector/trap_handler/trap_stack_corrupt 语义自明。
- 自洽：⚠️ TrapFrame 布局与 asm 保存序列**手工同步维护**（新增字段要改 asm 偏移）——无编译期校验，是脆弱点（注释已详述）。
- 正交：⚠️ `INTERRUPT_HANDLERS: SpinLock<Vec<Option<&'static dyn InterruptHandler>>>` 按中断号 O(1) 索引——**无共享 IRQ、无注册/注销所有权**；同 IRQ 二次注册静默覆盖（trap.rs:52）。
- 模块组织：⚠️ trap↔scheduler 依赖环（trap 调 scheduler 的 `scheduler/current_is_user/terminate_current`，scheduler 用 `trap::TrapFrame`）——TrapFrame 更接近共享基础设施，应归属独立底层或 scheduler。

**问题与建议**

1. 为 TrapFrame 布局加编译期断言（与 asm 保存尺寸核对）。
2. `register_interrupt_handler` 增加重复注册检测/错误返回。
3. 依赖环处理：TrapFrame 移出 trap 模块或抽为 `core/` 层（见跨模块综合）。

**结论**：★★★ 分发路径清晰健壮；注册表所有权与依赖环待完善。

---

## 10. lock

**职责**：六种同步原语（互斥 4 + 惰性 2），中断安全分级，多核预留。

**公共 API**

| 原语 | API | 备注 |
|------|-----|------|
| `SpinLock<T>` | `new` / `lock -> SpinLockGuard` / `try_lock`(dead) | 关中断（TrapGuard） |
| `BareLock<T>` | `new` / `unsafe lock` / `unsafe try_lock`(dead) | 不关中断，unsafe 强制约束 |
| `RwLock<T>` | `new` / `read` / `write` | 写者优先 |
| `RelLock<T>` | `new` / `lock`（同 hart 可重入） | 靠 hal::cpu::hart_id |
| `OnceLock<T>` | `new` / `get` / `set -> Result<(),T>` / `get_or_init` / `is_initialized` | 读路径无锁 |
| `LazyLock<T>` | `new(init: fn() -> T)` / `force` + Deref | 全 dead_code 预留 |
| `TrapGuard` | `unsafe save()`（pub(crate)） | 锁框架内部 |
| `lock_debug!` | feature-gated 宏（lock/log.rs） | — |

**五维评估**

- 命名/原语：✅ 全项目最佳模块——六原语命名规范、guard 语义一致（Deref/DerefMut + Drop 释放）、`BareLock::lock` unsafe 在类型层强制"不从中断上下文获取"。
- 自洽：✅ 锁层级（lock/mod.rs:17-27 KERNEL_SPACE→bus→INTERRUPT_HANDLERS）文档化，guard 携带 !Send 强制同 hart 释放；TrapGuard 收敛关中断逻辑（不重复写 CSR）。
- 正交：✅ 互斥/惰性 × 中断安全两维度划分清晰。
- 轻微：`LazyLock` 全 dead_code（lock/mod.rs:40 自述"可用暂未使用"）；`try_lock` 均预留 dead；`RwLock` 无 try_read/try_write（有写者优先，无非阻塞面）。

**结论**：★★★★ 命名、类型约束、文档化层级俱佳；仅剩 LazyLock/try_lock 预留待启用。

---

## 11. log

**职责**：五级日志 + 三层过滤（编译期/全局/模块）+ 双重输出（console + ring buffer）。

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `LogLevel` | `Error..Trace`（repr(u8)） | ✅ |
| `set_max_level` / `max_level` | 读写对称 | ✅ |
| `set_module_rules` | `fn (&'static [ModuleRule])` | ✅ |
| `Clock` trait | `now() -> u64` | ✅ |
| `CsrClock` / `CSR_CLOCK` | 读 time CSR | ✅ |
| `init_timestamp` | `fn (&'static dyn Clock, freq: u64)` | ✅ |
| `log_read` | `fn (offset: usize, buf: &mut [u8]) -> usize` | ✅ |
| `_log` | doc(hidden)，宏入口 | ✅ |
| 宏 | `error!`/`warn!`/`info!`/`debug!`/`trace!`/`log!` | ✅ |

**五维评估**

- 原语：✅ 三层过滤设计完整（编译期常量裁剪→AtomicU8 全局→OnceLock 规则表最长前缀匹配）；ring buffer 让早期日志不丢。
- 自洽：✅ `log_read(offset, buf)` 签名与 `File::read(offset, buf)` 巧合一致（dev/log.rs:20 直接透传）——无额外包装。✅ `init_timestamp` 在驱动就绪前注册（CSR_CLOCK），时间戳覆盖全启动。
- 命名：轻微——`init_timestamp` 未用 `set_` 范式（`set_clock_source` 更贴约定 1）；`_log` 下划线前缀约定俗成（doc(hidden)）。
- 模块组织：✅ 无反向依赖（log→print→…单向）。

**结论**：★★★★ 设计完整一致；仅命名微瑕。

---

## 12. print

**职责**：按特权级分层的输出原语（M-mode SBI / S-mode UART）。

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `MWriter` | ZST，SBI ecall 逐字节 | ✅ |
| `SWriter` / `S_WRITER` | SpinLock<SWriter>（pub(crate)） | ✅ |
| `print::init` | `fn ()`（SBI→UART 切换） | ✅ |
| 宏 | `print!`/`println!`/`mprint!`/`mprintln!` | ✅ |

**五维评估**

- 命名：✅ `MWriter`/`SWriter` 前缀表特权级，约定内。
- 原语：✅ boot 早期 MWriter 始终有效 → 输出路径无分支；`\r\n` 转换在 writer 层统一。
- 模块组织：⚠️ **依赖倒置**——`print::init` 直接查 `driver::serial::console()`（print.rs:102），而 `log→print→driver::serial→filesystem::traits`（File trait 定义）——基础输出层依赖驱动层与 VFS trait；若 serial 未 probe，靠"保持 SBI 输出"降级（print.rs:103-105，可接受但耦合实存）。
- 自洽：⚠️ U-mode 输出路径声明未实现（print.rs:6,17 注释——"将来用户态 ecall 陷入内核"），与架构评审一致。

**问题与建议**

1. 输出依赖注入：`print::init` 消费抽象 writer 接口（如 `&'static dyn fmt::Write` 参数），由 init 编排层注入 serial 实例，切断 print→driver 依赖。
2. U-mode 声称在落地前标注"未实现"或移入 ROADMAP。

**结论**：★★☆ 输出路径无分支设计好；依赖倒置与未落地声称待修。

---

## 13. panic

**职责**：panic 处置——直写 UART（绕过锁）+ CSR 转储 + frame-pointer 回溯 + SBI 关机。

**公共 API**

| API | 签名 | 状态 |
|-----|------|------|
| `PanicVerbosity` | `Normal/Full` | ✅ |
| `set_verbosity` | `fn (PanicVerbosity)` | ✅ |
| `panic_handler` | `#[panic_handler]` | ✅ |

**五维评估**

- 原语：✅ 绕过所有锁直写 UART（经 UART `write_byte` pub(crate) 通道）——死锁安全设计正确；`PanicVerbosity` 分级输出。
- 自洽：⚠️ backtrace 依赖 `scheduler::TASK_STACK_BASE/STACK_SIZE` 常量（panic.rs:127-129）——panic 模块依赖 scheduler 常量（轻微反向耦合，可抽为公共内存布局常量）。
- 命名：✅ `set_verbosity` 读对称缺失（无 `verbosity()` pub——合理，内部细节）。

**结论**：★★★ 死锁安全路径正确；仅常量归属微瑕。

---

## 14. init / main

**职责**：init——四阶段引导编排（allocator→space→driver→trap / devfs→print→log / 中断使能）；main——入口与 demo。

**公共 API**

| 模块 | API |
|------|-----|
| init | `InitError{Memory(table::MapError), Driver(driver::DriverError)}`；`Result<T>` 别名；`unsafe run() -> Result<()>` |
| main | `early`/`main`（入口）；`task`（基础任务）；`DEMO_*` 7 开关；`demo_*` 7 个演示任务（均 `#[allow(dead_code)]`） |

**五维评估**

- init：✅ 四阶段注释清晰、错误包装正确（InitError 两变体对齐阶段）；中断使能收敛在 Phase 3 单一入口（sie::set 三连 + sstatus::set），驱动不触碰 sie——好的编排边界。
- main：⚠️ **demo 代码与内核入口混杂**——main.rs 386 行中约 250 行是 7 个 demo 任务 + 开关（main.rs:68-77,193-354），`#[allow(dead_code)]` 压在大量代码上；demo 开关是编译期常量但每次只跑一个，属"测试代码长驻生产入口"。
- 命名：✅ `InitError::Memory(…)/Driver(…)` 语义清晰。

**问题与建议**

1. demo 抽离为 `src/demos/`（feature-gated 或独立 demo 入口），main.rs 只留基础任务。
2. `InitError` 的 `#[allow(dead_code)]` 随错误字段读取后移除。

**结论**：init ★★★ 编排清晰；main ★★☆ demo 混杂待分离。

---

## 15. 跨模块综合

### A. 设计缺陷（影响扩展/正确性，优先处理）

| # | 问题 | 证据 | 影响 |
|---|------|------|------|
| A1 | **filesystem ↔ driver 依赖环** | devfs→`serial::all()`（dev/mod.rs:32）；UART impl `File`（uart16550.rs:128） | VFS 层与驱动层互为依赖；virtio-blk 等新驱动会扩散同型环 |
| A2 | **trap ↔ scheduler 依赖环** | trap 调 scheduler 函数（trap.rs:227,274）；scheduler 用 `TrapFrame`（scheduler.rs:30） | TrapFrame 归属不清；分层被打破 |
| A3 | **memory::fault ↔ scheduler 互依** | fault→`current_space()`（fault.rs:113 经 trap）；scheduler→memory::switch_space | 内存与调度双向耦合 |
| A4 | **U-mode 声称 vs 实现** | `TaskKind::User`（scheduler.rs:49-54）但 SPP=S（scheduler.rs:552）；trap.rs:283-287 ecall→terminate | 用户态落地即死；文档误导 |
| A5 | **sleep 单位三方不一致** | scheduler.rs:422 注释 vs :429-431 实现 vs 文档 | 命名即语义违反，调用方易错 |
| A6 | **Mmio 单实现者伪抽象 + 同名歧义** | 仅 UART 两实现（uart16550.rs:120）；约定 6 为此支付固有方法改名成本 | 抽象名不副实，read/write 语义割裂 |
| A7 | **外部中断错误类型裸字符串** | interrupt.rs:53 `Result<(), &'static str>` | 唯一违反"错误码语义即行为" |
| A8 | **log→print→driver→filesystem 依赖倒置** | print.rs:102 查 serial::console；serial 依赖 File trait | 基础输出层依赖高层类型 |

### B. 约定违反（对照基线，第二优先）

| # | 问题 | 证据 | 违反约定 |
|---|------|------|----------|
| B1 | `static mut PLATFORM/SAVED_DTB` | config.rs:43,46,96-100 | 约定 8/9（OnceLock 用途、无 pub static mut） |
| B2 | `sleep(ticks)` 命名 | scheduler.rs:429 | 约定 2（命名即语义） |
| B3 | `MapError` pub(crate) 泄漏 pub API | space.rs:141 等 | 暴露面完整性 |
| B4 | `map_device(base: usize)` 丢弃强类型 | memory/mod.rs:66 | 强类型边界 |
| B5 | 双 UART 重复 `\r\n` 逻辑 | uart16550.rs:143-168 / sifive_uart.rs:126-151 | DRY |
| B6 | `bus::find_all` dead 与 serial 注册表双查询面 | bus.rs:127-135 | 机制重复 |

### C. 不一致 / 冗余（第三优先，清理）

| # | 问题 | 证据 |
|---|------|------|
| C1 | `VirtAddr::new`/`from_raw` 并存（new dead） | addr.rs:33-50 |
| C2 | `PageTableEntry::new_branch` dead（walk_mut 手动 set） | entry.rs:62-66, table.rs:128 |
| C3 | `Driver::name` dead | traits.rs:36 |
| C4 | `Dtb::find_*` dead + device::probe 重复解析 | dtb.rs:50-77, device.rs:128-154 |
| C5 | `map` vs `map_region` 双方法 | space.rs:141,166 |
| C6 | `sstatus::SPP` 裸 const vs bitflags 成员 | csr.rs:38 vs 27-33 |
| C7 | main.rs demo 混杂（约 250/386 行） | main.rs:68-77,193-354 |
| C8 | `handle_timer` 默认实现与 clint 覆写重复 | interrupt.rs:16-18, clint.rs:52-55 |
| C9 | `root_page() -> u64` vs `switch_space(usize)` | space.rs:269, memory/mod.rs:37 |

### 建议优先级

1. **先 A（地基）**：A4/A5 命名与语义修复（零成本、立即受益）→ A1/A2/A3 依赖环收敛（TrapFrame 下沉、文件能力出口剥离、current_space 走显式接口）→ A7 错误类型统一 → A6/A8 抽象收敛。
2. **再 B（约定恢复）**：B1 OnceLock 化 → B3/B4 错误与类型可见性 → B5 去重 → B6 机制收敛。
3. **后 C（清理）**：删 dead API、收敛双构造器/双方法、demo 抽离、SPP 封装。

> 注：以上均为**评审建议**，实施与否、顺序由项目路线决定；本评审不修改任何代码。
