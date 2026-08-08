// 块设备硬件能力契约 — BlockDevice trait + 注册表
//
// 纯硬件能力（零依赖）+ 注册表（与 hal::rtc / hal::interrupt 的 register/get
// 同构：OnceLock 写一次读多次）。驱动（driver/block/）实现本 trait，probe 时经
// `register()` 注册；服务集成（file::io::block 的 BlockFile、file::fs）经
// `get()` 查询（未注册返回 None）。
//
// 对应 Linux：block_device_operations 的注册表面。`read_block`/`write_block` 为
// **同步**（返回即完成）：驱动内部按当前 `sstatus::SIE` 选择 wfi（SIE=1）或轮询
// （SIE=0），调用方零心智负担。`buf` 长度须 ≥ `block_size()`；驱动内部经 bounce
// buffer 拷贝，buf 可为任意 VA（任务栈/堆/用户缓冲，不必是恒等页）。

use crate::lock::OnceLock;

/// 块设备能力契约 — 驱动实现的硬件接口（object-safe，Linux `block_device_operations` 对应物）。
pub trait BlockDevice: Send + Sync {
    /// 块大小（字节），如 virtio-blk 的 512。
    fn block_size(&self) -> usize;

    /// 块总数。
    fn block_count(&self) -> usize;

    /// 同步读取整块到 `buf`（`buf.len()` ≥ [`Self::block_size`]）。
    fn read_block(&self, block: u32, buf: &mut [u8]);

    /// 同步写入整块自 `buf`（`buf.len()` ≥ [`Self::block_size`]）。
    fn write_block(&self, block: u32, buf: &[u8]);

    /// PLIC 中断号 — 请求完成通知（BlockIrqHandler 注册用）。
    fn interrupt_number(&self) -> u32;

    /// ACK 完成中断 — 清设备中断位，防 PLIC 中断挂起（handler 归设备）。
    fn ack_interrupt(&self);
}

static BLOCK_DEV: OnceLock<&'static dyn BlockDevice> = OnceLock::new();

/// 注册块设备 — 驱动 probe 时调用一次。
pub fn register(dev: &'static dyn BlockDevice) {
    if BLOCK_DEV.set(dev).is_err() {
        crate::warn!("block device already registered (block::register called more than once)");
    }
}

/// 获取已注册的块设备（未注册返回 None）。
pub fn get() -> Option<&'static dyn BlockDevice> {
    BLOCK_DEV.get().copied()
}
