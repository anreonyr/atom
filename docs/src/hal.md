# hal 模块 API 契约

> 本文件是 `src/hal/` 模块的权威 API 契约：公共签名、数据命名、引导流程与约定。
> 代码变更须与本文档保持同步（对应 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例；新模块文档按模板创建。
> 从 CLAUDE.md「HAL trait divisions」迁出。

## 1. 职责

硬件能力契约集合（原子，零依赖服务层）：hal = 全部硬件能力契约（纯 trait + 注册表）的集合。
驱动实现这些 trait，服务集成层（`file::io` 等）按需适配。

边界：

- **不做驱动实现**：具体型号（16550 / PLIC / CLINT / Goldfish RTC）在 `driver/`（见 `driver.md`）。
- **不做终端集成**：`Uart` 契约在 `hal/uart.rs`，服务集成在 `file::io/uart.rs`（`pub use` 重导出保持 driver 兼容）。

## 2. 引导流程

驱动 probe 时注册（如 CLINT probe → `register_internal()`，UART probe → `trap::register_interrupt_handler()`）。

## 3. 公共 API 面

### 3.1 Trait divisions

驱动实现以下一个或多个 trait：

| Trait | Implemented by | Registration |
|-------|---------------|--------------|
| `Driver` | Uart16550, Plic, Clint (model drivers) | Bus match → `probe` |
| `InternalInterrupt` | Clint (timer + IPI) | `hal::interrupt::register_internal()` |
| `ExternalInterrupt` | Plic (enable/claim/complete) | `hal::interrupt::register_external()` |
| `InterruptHandler` | Uart (per-device, blanket 适配自 `trait Uart`) | `trap::register_interrupt_handler()` |
| `fmt::Write` | Uart（经 `file::io::uart::UartWriter` 本地包装，注册表层构造） | `file::io::device::register` 联动时以 `&'static dyn Write` 入设备表 |
| `File` | Uart（经 `file::io::uart::UartFile`）、io 标准流（Stdin/Stdout）、devfs 节点（Null/Zero） | `file::registry::register`（devfs 枚举建节点） |

### 3.2 文件分布

| 文件 | 内容 |
|------|------|
| `interrupt.rs` | `InternalInterrupt` / `ExternalInterrupt` / `InterruptHandler`（+ 注册表） |
| `uart.rs` | `Uart` 硬件能力契约（纯 trait；服务集成在 `file::io/uart.rs`） |
| `rtc.rs` | `Realtime` 硬件能力契约 + 注册表（`register`/`epoch_secs`，未注册返回 None） |
| `csr.rs` | S-mode CSR wrappers (sstatus, sie, stvec, scause, ...) |
| `cpu.rs` | `HartId` (single-hart stub) |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| trait 描述能力而非动作 | `Driver::name/compatibles/probe`、`Uart`/`ExternalInterrupt` 是能力契约，不是动作函数 |

## 5. 生命周期与并发

- 注册表为 `&'static` 单例（`Box::leak`），生命周期与内核等同。
- RTC 注册表未注册时 `epoch_secs` 返回 `None`（查询语义，调用方降级）。

## 6. 变更记录

- 2026-08-08：从 CLAUDE.md 迁出（HAL trait divisions）。
