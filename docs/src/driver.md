# driver 模块 API 契约

> 本文件是 `src/driver/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「Device model / Driver pattern / DTB device discovery / PLIC registers」迁出。

## 1. 职责

Linux **hub / device / driver** 模型：DTB 发现设备 → `compatible` 匹配驱动 → deferred probe；
实例存于设备，跨驱动依赖经 `hub::find`。一个驱动型号一个文件，只实现 Uart / Interrupt 等能力。

边界：

- **不做服务注册表**：实例挂在 `Device` 上；无独立全局访问器（无 `UART()`/`CLINT()`/`PLIC()`）。
- **不做能力契约定义**：`Uart`/`InternalInterrupt` 等契约在 `hal/`（见 `hal.md`）；驱动只实现之。
- **不做终端集成**：UART 的 Write/File 视图与 /dev/consoleN、/dev/uartN 注册在 `file::io::console`
  （见 `file.md`），driver 层看不到 `File` 类型（no console bridge type）。

## 2. 引导流程

```
hub::init()
  └─ device::probe()               ← 走 platform::config::dtb() 句柄，产出 Vec<Device>（compatible/base/size/interrupt）
       └─ hub 逐设备按 compatible 匹配 Driver
            └─ deferred probe 循环：DriverError::Deferred → 重试；Stuck → 终止（死锁）
                 └─ probe 成功 → dev.set_instance(实例) → Bound
```

`probe(&Device)` 自包含（以 uart16550 为例）：

```rust
// uart/uart16550.rs（实现 `hal::byte_channel::ByteChannel`）
pub struct Uart16550Driver;
impl Driver for Uart16550Driver {
    fn name(&self) -> &'static str { "uart16550" }
    fn compatibles(&self) -> &'static [&'static str] { &["ns16550a"] }
    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        memory::map_device(dev.base, dev.size, page::allocator())?;  // ① MMIO 映射
        let uart = Box::leak(Box::new(Uart16550::new(dev.base, irq)));
        dev.set_instance(uart);                                       // ② 实例挂设备
        uart.init()?;                                                 // ③ 硬件初始化
        console::register(uart);                                      // ④ 注册终端核心（Console/InputHandler/RawFile）
        let plic = hub::find::<Plic>().ok_or(DriverError::Deferred)?; // ⑤ 依赖：deferred
        // ... 中断路由（plic.enable + uart.enable_interrupt）
        Ok(())
    }
}
pub static DRIVER: &dyn Driver = &Uart16550Driver;
```

## 3. 公共 API 面

### 3.1 类型

| 类型 | 语义 |
|------|------|
| `Device` | DTB 设备实体：`compatible`/`base`/`size`/`interrupt` + 生命周期状态（`Unbound`/`Deferred`/`Bound`/`Unsupported`）；probe 产物经 `set_instance()` 存放（Linux `dev_set_drvdata`） |
| `DeviceState` | 设备生命周期状态 |
| `Driver` | 数据型 trait：`name()` / `compatibles()`（id_table）/ `probe(&Device)`；每个型号一个 impl，同类型不同型号（16550 / PL011）各自独立驱动 |
| `DriverError` | `Deferred`（重试）/ `Stuck`（死锁终止），错误消息带 `&'static str` 上下文 |
| `Hub` | compatible 匹配 + deferred probe + `hub::find::<T>()`（实例按具体类型 downcast） |

### 3.2 常量 / 角色注册表

- 每个型号文件导出 `pub static DRIVER: &dyn Driver`，按角色目录聚合（`uart::DRIVERS`、`controller::DRIVERS`、`rtc::DRIVERS`）。
- 多实例枚举走角色注册表：`file::io::console::count()` / `file::io::console` 终端核心；`hub::find` 只返回首个匹配。

### 3.3 PLIC 寄存器（S-mode context=1）

| Offset | Register |
|--------|----------|
| `+ source*4` | Priority per source |
| `+ 0x2080 + word*4` | Enable (bitmap per word) |
| `+ 0x201000` | Priority threshold |
| `+ 0x201004` | Claim / Complete |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 读写对称 | `set_instance` / instance 字段（读 = 字段名） |
| 查询 API 用查找动词，返回 `Option` | `hub::find::<T>() -> Option<&T>`；调用方 `ok_or(DriverError::Deferred)` 决定降级 |
| 错误码语义即行为 | `Deferred` → 重试，`Stuck` → 终止 |
| 不用缩写 | `instance` 而非 `drvdata`；`compatibles` 全称 |
| 寄存器访问走固有方法 | `read_reg`/`write_reg`（与 `File`/`fmt::Write` 的 `read`/`write` 命名隔离，避免 E0034） |

## 5. 生命周期与并发

- 实例经 `Box::leak` 得 `&'static T`，设备永生；`set_instance` 挂到对应 `Device`。
- 跨驱动依赖在 probe 时经 `hub::find` 解析（如 UART 依赖 Plic），未就绪返回 `Deferred` 交给 hub 重试。
- 同一 driver 型号处理 N 个同 compatible 设备：一个 `Device` 对应一个 DTB 节点，各自独立 instance 字段。
- DTB 发现只发生一次（`hub::init`）；`device.rs::probe()` 复用 `platform::config::dtb()` 句柄。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（Device model / Driver pattern / DTB device discovery / PLIC registers）。
