# platform 模块 API 契约

> 本文件是 `src/platform/` 模块的权威 API 契约：公共函数签名、数据命名、
> 引导流程与约定。代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。

## 1. 职责

引导早期从 DTB 探测全局硬件参数（DRAM / timebase / hart 数），提供只读全局配置；
零分配解析 FDT（`Dtb`/`Node`）。设备级发现不在此模块——由 `driver::device::probe()`
复用本模块保存的 DTB 句柄完成。

边界：

- **只做"硬件事实"读取**：`timebase_frequency` 来自 DTB `/cpus` 节点（或 QEMU virt 回退），不是 SBI 返回值；与 `sbi::set_timer` 的协作是调用方组合（`clint.rs`：`abs = read() + frequency()`）。
- **不承担 M-mode 服务**：SBI ecall（`sbi` 模块）与本模块无依赖关系。

## 2. 引导流程

```
_start → early(hartid, ptr)      ← ptr 来自 OpenSBI 启动协议 (a1)
   ├─ platform::init(ptr)        ← 校验 DTB 一次；失败即打印 + qemu-virt 回退
   │    ├─ Dtb::new → 保存句柄（OnceLock<Dtb>）
   │    └─ probe_global(&Dtb) → Config（OnceLock<Config>）
   ├─ platform::get()                ← 只读访问 DRAM/timebase 等
   └─ main → init::run()
        └─ driver::device::probe()   ← platform::config::dtb() 复用句柄，重解析设备
```

DTB 校验**只发生一次**（`init`），全局探测与设备发现共享同一句柄。

## 3. 公共 API 面

### 3.1 函数

| API                     | 签名                                                                      | 语义与契约                                                                                                                            |
| ----------------------- | ----------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `init`                  | `pub unsafe fn init(ptr: usize)`                                        | 引导早期单 hart 调用恰好一次；`ptr == 0` 或 DTB 无效 → 内部打印错误 + `qemu_virt` 回退，**不返回 `Result`**（错误即时输出，SBI writer boot 即可用）；成功则保存 DTB 句柄并解析全局参数 |
| `get`                   | `pub fn get() -> &'static Config`                                       | **访问器**（非查找）：`init` 后必就绪，未 init 则 panic。与「查询用查找动词返回 `Option`」的规范有意不同——调用方 10+ 处均依赖其不可失败性                                         |
| `dtb`                   | `pub(crate) fn dtb() -> Option<&'static Dtb>`                           | 查找语义：返回校验过的 DTB 句柄，缺失/无效为 `None`（设备发现走回退列表）。非破坏——句柄全生命周期有效，无 `take`                                                              |
| `Dtb::new`              | `pub unsafe fn new(ptr: usize) -> Result<Self, DtbError>`               | 校验 FDT 头；`ptr` 须指向有效 FDT 数据                                                                                                      |
| `Dtb::walk`             | `pub fn walk() -> Walk<'_>`                                             | 深度优先遍历全部非根节点（跳过根与 `/chosen`）                                                                                                     |
| `Node::name`            | `fn name(&self, dtb: &Dtb) -> &'a str`                                  | 节点名（如 `"uart@1000000"`）                                                                                                          |
| `Node::property_reg`    | `fn property_reg(&self, dtb: &Dtb, index: usize) -> Option<(u64, u64)>` | reg 第 `index` 组 `(base, size)`，按 `#address-cells`/`#size-cells` 解包                                                               |
| `Node::property_u32`    | `fn property_u32(&self, dtb: &Dtb, name: &str) -> Option<u32>`          | 4 字节属性                                                                                                                           |
| `Node::property_string` | `fn property_string(&self, dtb: &Dtb, name: &str) -> Option<&'a str>`   | 字符串属性（截断到首个 `\0`）                                                                                                                |
| `Node::property_raw`    | `fn property_raw(&self, dtb: &Dtb, name: &str) -> Option<&'a [u8]>`     | 原始属性字节（预留）                                                                                                                       |

### 3.2 数据（`Config`，全 pub 字段）

| 字段 | 类型 | 语义 |
| ------ | ------ | ------ |
| `dram_base` | `usize` | DRAM 物理基址 |
| `dram_size` | `usize` | DRAM 总大小 (bytes) |
| `timebase_frequency` | `u64` | 定时器频率 (Hz)——**全称拼写**，跨模块一致（`qemu_virt::TIMEBASE_FREQUENCY`、`Clint::timebase_frequency`；禁止 `freq` 缩写） |
| `stack_reserve` | `usize` | 内核栈保留大小（DRAM 末尾向下预留） |
| `hart_count` | `usize` | CPU / hart 数量（`hart` 为 RISC-V 术语，非缩写） |

### 3.3 常量

| 名称 | 值 | 语义 |
| ------ | ----- | ------ |
| `PAGE_SIZE` | `4096` | RISC-V 页大小（所有 Sv 模式通用） |
| `qemu_virt::DRAM_BASE` / `DRAM_SIZE` | `0x8000_0000` / 8 MiB | DTB 缺失时的回退 DRAM |
| `qemu_virt::UART_BASE` / `SIZE` / `INTERRUPT` | `0x1000_0000` / `0x1000` / 10 | 回退 UART |
| `qemu_virt::CLINT_BASE` / `SIZE` | `0x0200_0000` / `0x1_0000` | 回退 CLINT |
| `qemu_virt::PLIC_BASE` / `SIZE` | `0x0C00_0000` / `0x30_0000` | 回退 PLIC（覆盖 S-mode 上下文） |
| `qemu_virt::TIMEBASE_FREQUENCY` | `10_000_000` | 回退定时器频率（10 MHz） |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
| ------ | ----------- |
| 读写对称（读方法 = 字段名，无 `get_` 前缀） | `get()`、`dtb()`（对应静态 `PLATFORM`/`DTB`） |
| 不用缩写 | `timebase_frequency` / `TIMEBASE_FREQUENCY`（曾用 `freq` 缩写，已统一）；`dtb`/`hart` 为标准术语 |
| 查询 API 用查找动词，返回 `Option` | `dtb() -> Option<&'static Dtb>`；`Dtb::new -> Result` |
| 错误消息带 `&'static str` 上下文 | `println!("[platform] DTB parse failed, ...")` |
| 类型名与模块路径对齐 | 类型 `Config`（`platform::Config`），模块 `config`，函数 `platform::get()`——不再有 `Platform` 与模块名错位 |
| 全局只读态用 `OnceLock` | `PLATFORM`/`DTB` 均为 `OnceLock`（写一次读多次，allocator 前可用，无 unsafe 读路径） |

## 5. 生命周期与并发

- `OnceLock`：`init` 前 `get()`/`dtb()` 返回 `None`/panic；`init` 后只读无锁（Acquire load）。
- `init` 的 `unsafe`：源于 `Dtb::new` 读裸物理内存（`dtb_ptr` 有效性由调用方保证），非并发问题。
- DTB 物理内存由 OpenSBI 保留、全生命周期有效——句柄与 `&'static str`（`compatible`）可安全长期持有。

## 6. 变更记录

- 重构：`static mut PLATFORM/SAVED_DTB` → `OnceLock`；`take_saved_dtb`（破坏性）→ `dtb()`（只读）；
  `report_probe_error()`/`PROBE_ERROR` 删除（错误改 `init` 内即时打印）；`Dtb::find_*` 删除（无消费者，
  设备发现全量遍历不适用按名查询）；`Platform` → `Config`；`TIMEBASE_FREQ` → `TIMEBASE_FREQUENCY`；
  `Clint::timebase_freq` → `timebase_frequency`。
