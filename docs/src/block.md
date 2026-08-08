# block 模块 API 契约（块设备：能力契约 + virtio-blk 驱动 + BlockFile）

> 本文件是块设备子系统的权威 API 契约。覆盖三层：`hal::block`（能力契约）、
> `driver/block/`（virtio-blk 驱动）、`file::io::block`（服务集成）。
> 代码变更须与本文档保持同步（对照 CLAUDE.md「API naming conventions」）。
> 本文档是 `docs/src/template.md` 模板的一个实例。

## 1. 职责

块设备接入：`hal::BlockDevice` 能力契约（零依赖原子）→ virtio-blk 驱动实现 →
`file::io::block` 服务集成（`BlockFile` 的 `File` 视图 + 完成中断 handler）。复用
「终端核心提炼」的 **设备 → File 接缝模板**（能力契约 → 非泛型 register → File
视图 → 中断 handler 归设备），块设备是 `File::read/write(offset, …)` 随机访问参数
的主战场（与 Console 忽略 offset 的流式不同）。

边界：

- **不做具体文件系统**：极简 FS（superblock/inode/目录）在 `file::fs`（见 fs.md），
  只消费 `hal::BlockDevice`。
- **不做终端集成**：`ByteChannel`（字节收发）与 `BlockDevice`（块读写）是不同能力
  契约，virtio-console 等未来设备实现前者、复用终端核心。
- **不做 DMA 抽象**：内核恒等映射（PA==VA），virtqueue/bounce 用 frame 物理页直连；
  任务栈等非恒等 VA 经 bounce 拷贝化解（bounce 是必须的）。

## 2. 引导流程

```
driver probe（Phase 1）: VirtioBlkDriver::probe
  ① hub::find::<Plic>()         — 依赖前置，未就绪 Deferred
  ② driver::map_mmio(dev)       — 恒等映射 virtio-mmio 区域
  ③ 校验 magic/version/device_id — DeviceID!=2（空槽/console）→ 跳过不绑定
  ④ 特性协商：复位→ACKNOWLEDGE→DRIVER→VERSION_1→FEATURES_OK
  ⑤ frame 分配一页 → 平铺 virtqueue + bounce → 队列激活 → DRIVER_OK
  ⑥ 读配置区（capacity/blk_size）→ 构造 VirtioBlk → set_instance
  ⑦ file::io::block::register(blk) — hal 单例 + /dev/block0 + BlockIrqHandler
  ⑧ PLIC 路由（set_priority + enable）

QEMU 启动（`-machine virt`，QEMU 11+ 必须 force-legacy=off）：
  qemu-system-riscv64 -machine virt -bios default \
    -kernel target/riscv64gc-unknown-none-elf/debug/atom -nographic \
    -global virtio-mmio.force-legacy=off \
    -drive if=none,id=hd0,file=disk.img,format=raw \
    -device virtio-blk-device,drive=hd0
```

> **QEMU 关键坑**：virtio-mmio 传输默认 `force-legacy=on`（legacy 模式）——不提供
> VIRTIO_F_VERSION_1、忽略现代队列地址寄存器（0x80+），请求永不完成（used ring 不动）。
> 必须 `-global virtio-mmio.force-legacy=off` 强制 modern（version=2 才提供 VERSION_1）。
> 此外 QEMU virt 生成 8 个 virtio-mmio 槽，仅一个挂块设备；其余空槽 device_id=0，
> 驱动 probe 跳过（不 Stuck）。

## 3. 公共 API 面

### 3.1 hal::block — 能力契约（`src/hal/block.rs`，零依赖原子）

```rust
pub trait BlockDevice: Send + Sync {
    fn block_size(&self) -> usize;        // 块大小（virtio-blk 512）
    fn block_count(&self) -> usize;       // 块总数
    fn read_block(&self, block: u32, buf: &mut [u8]);   // 同步，返回即完成
    fn write_block(&self, block: u32, buf: &[u8]);      // 同步，返回即完成
    fn interrupt_number(&self) -> u32;    // PLIC 中断号（完成通知）
    fn ack_interrupt(&self);              // ACK 设备中断位（防 PLIC 挂起）
}
pub fn register(dev: &'static dyn BlockDevice);   // 驱动 probe 调用一次
pub fn get() -> Option<&'static dyn BlockDevice>; // 未注册返回 None
```

单例注册表仿 `hal::rtc`（OnceLock 写一次读多次）。

### 3.2 driver/block — virtio-blk 驱动（`src/driver/block/virtio_blk.rs`）

| 类型 | 语义 |
|------|------|
| `VirtioBlk` | 设备实例（MMIO base + irq + block_size/count + `SpinLock<VirtQueue>`）；`impl BlockDevice + unsafe impl Send/Sync` |
| `VirtioBlkDriver` | `impl Driver`；compatibles = `["virtio,mmio"]`；name = `"virtio-blk"` |
| `DRIVERS` | 角色目录驱动表（hub 聚合） |

**virtio-mmio 现代寄存器偏移**（Linux `virtio_mmio.h` 布局，实测确认 Status@0x70）：

| 偏移 | 寄存器 | 偏移 | 寄存器 |
|------|--------|------|--------|
| 0x000 | MagicValue (=0x74726976) | 0x050 | QueueNotify |
| 0x004 | Version (QEMU 报 1；现代布局仍生效) | 0x060/0x064 | InterruptStatus / InterruptACK |
| 0x008 | DeviceID (=2 block) | 0x070 | **Status** |
| 0x010/0x014 | DeviceFeatures / Sel | 0x080/0x084 | QueueDescLow / High |
| 0x020/0x024 | DriverFeatures / Sel | 0x090/0x094 | QueueDriverLow / High |
| 0x030/0x034/0x038 | QueueSel / NumMax / Num | 0x0a0/0x0a4 | QueueDeviceLow / High |
| 0x044 | QueueReady | 0x100+ | 配置区（capacity u64 @+0，blk_size u32 @+24） |

**virtqueue 布局**（一页 4K 恒等 DMA，`frame::allocator()`，PA==VA）：

```
+0x000  descriptor table[32]   (16B × 32)
+0x200  available ring
+0x250  used ring
+0x360  header（virtio_blk_req 16B）
+0x370  status byte
+0x400  bounce buffer（block_size）
```

请求 = 3 描述符链（header → bounce → status）；完成检测经
`wait_event(Event::Block, done)`——锁内 SIE=0 自动走轮询分支，无锁内核任务走 wfi
中断唤醒；`read_block`/`write_block` 对任意 buf 都经 bounce 拷贝（任务栈 VA 非恒等）。

### 3.3 file::io::block — 服务集成（`src/file/io/block.rs`）

| 类型/API | 语义 |
|----------|------|
| `BlockFile` | `impl File`：read/write **用 offset 随机块访问**（首尾部分块 RMW）；`size()` = 整盘字节；seek 支持 `End` |
| `BlockIrqHandler` | `impl InterruptHandler`：完成中断 → `ack_interrupt()` + `signal_event(Event::Block)`（handler 归设备） |
| `register(&'static dyn BlockDevice)` | 非泛型：hal 单例 + `/dev/block0` + handler 注册（仿 `file::io::console::register`） |

## 4. 命名框架（对照 CLAUDE.md「API naming conventions」）

| 约定 | 本模块落点 |
|------|-----------|
| 寄存器访问走固有方法 | `read32`/`write32`（MMIO，避免与 `File::read/write` 撞名） |
| trait 描述能力而非动作 | `BlockDevice` 是能力契约；驱动 probe 是动作（`Driver::probe`） |
| 查询 API 用查找动词 | `hal::block::get() -> Option<...>`；调用方降级（file::fs 无设备则无 /data） |
| 不用缩写 | `block_count`/`block_size` 全称；`interrupt_number` 而非 `irq_no` |

## 5. 生命周期与并发

- **恒等 DMA**：virtqueue 页经 `frame::allocator()` 分配后 `Box::leak` 于 `VirtioBlk`
  内永不释放；PA==VA 直连设备。QEMU 模拟器侧内存一致，无需显式 cache flush
  （教学内核假设一致内存）；`fence` 仍按 spec 在 avail 写后 / used 读前加。
- **队列串行化**：单请求在途——`read_block`/`write_block` 持 `SpinLock<VirtQueue>`
  （关中断，SIE=0 → wait_event 轮询完成）；中断 handler 不碰队列锁（只 ack +
  signal_event），无死锁。
- **bounce 必须**：任务栈 VA（0xC0000000）非恒等，`buf` 不可直接当 PA 给设备；
  驱动内部经 bounce 页拷贝。
- **非块 virtio 设备**：probe 对 DeviceID!=2 返回 Ok 不绑定（避免 hub Stuck）；
  空槽被记 Bound（该驱动认领但跳过）。

## 6. 变更记录

- 2026-08-08：M4——`hal::BlockDevice` + virtio-blk（现代 virtio-mmio，Linux 布局）+
  `BlockFile`（offset 随机访问）/`BlockIrqHandler`/`register`；完成经统一事件原语
  `Event::Block`；QEMU 须 `-global virtio-mmio.force-legacy=off`。
