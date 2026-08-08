// virtio-blk 驱动 — block/ 角色目录（现代 virtio-mmio，Version=2）
//
// VirtioBlkDriver 匹配 "virtio,mmio" 设备；probe 校验 DeviceID==2（block，非块
// virtio 设备返回 Ok 不绑定，避免 hub Stuck），特性协商（仅 VIRTIO_F_VERSION_1）、
// 分配一页恒等 DMA 内存平铺 virtqueue + bounce buffer、激活队列、读取块大小/容量，
// 构造 VirtioBlk 实例挂载后经 file::io::block::register 注册块服务（hal + /dev/block0
// + BlockIrqHandler），PLIC 路由。
//
// 完成通知：提交请求后经 `wait_event(Event::Block, done)` 等完成——持有队列 SpinLock
// 时 SIE=0 自动走轮询分支，无锁内核任务下走 wfi 中断唤醒。BlockIrqHandler 只 ack
// + signal_event（不碰队列锁），单请求在途（SpinLock 串行化提交）。
//
// 依赖方向：driver → hal::block（实现契约）+ driver（hub/map_mmio/traits）+ 调用
// file::io::block::register（仿 uart16550 调 console::register）。file 域不反向依赖。

use alloc::boxed::Box;
use core::alloc::Layout;

use crate::driver::controller::plic::Plic;
use crate::driver::device::Device;
use crate::driver::hub;
use crate::driver::traits::{Driver, DriverError};
use crate::hal::ExternalInterrupt;
use crate::lock::SpinLock;
use crate::memory::allocator::frame;

// ── virtio-mmio 寄存器偏移（Linux 现代布局，QEMU virt 实测）────────
//
// 实测（QEMU 11 `-machine virt` + virtio-blk-device）：Status 在 0x70（写 1 读回
// 1）、QueueNumMax 在 0x34（=1024）——即 Linux virtio_mmio.c 的现代偏移。注意
// Version 寄存器读回 1（QEMU 语义是传输代次非"legacy/现代"），故版本检查放宽。
const REG_MAGIC: usize = 0x00; // = 0x74726976 ("virt")
const REG_VERSION: usize = 0x04; // QEMU 报 1（传输代次）；现代偏移按 Linux 布局
const REG_DEVICE_ID: usize = 0x08; // = 2 (VIRTIO_ID_BLOCK)
const REG_DEVICE_FEATURES: usize = 0x10;
const REG_DEVICE_FEATURES_SEL: usize = 0x14;
const REG_DRIVER_FEATURES: usize = 0x20;
const REG_DRIVER_FEATURES_SEL: usize = 0x24;
const REG_QUEUE_SEL: usize = 0x30;
const REG_QUEUE_NUM_MAX: usize = 0x34;
const REG_QUEUE_NUM: usize = 0x38;
const REG_QUEUE_READY: usize = 0x44;
const REG_QUEUE_NOTIFY: usize = 0x50;
const REG_INTERRUPT_STATUS: usize = 0x60;
const REG_INTERRUPT_ACK: usize = 0x64;
const REG_STATUS: usize = 0x70;
const REG_QUEUE_DESC_LOW: usize = 0x80;
const REG_QUEUE_DESC_HIGH: usize = 0x84;
const REG_QUEUE_DRIVER_LOW: usize = 0x90;
const REG_QUEUE_DRIVER_HIGH: usize = 0x94;
const REG_QUEUE_DEVICE_LOW: usize = 0xa0;
const REG_QUEUE_DEVICE_HIGH: usize = 0xa4;

// 设备配置区起点（现代布局，Config 0x100 起）
const CONFIG_CAPACITY: usize = 0x100; // u64 sectors（512B 扇区）
const CONFIG_BLK_SIZE: usize = 0x100 + 24; // u32 bytes

// 设备状态位
const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_DRIVER_OK: u32 = 16;

/// virtio 块设备 ID（VIRTIO_ID_BLOCK）。
const VIRTIO_ID_BLOCK: u32 = 2;

// ── virtqueue 布局（单页 4K 平铺，恒等 PA==VA）─────────────────
//
//   +0x000  descriptor table[QSZ]  (16B × QSZ)
//   +0x200  available ring
//   +0x250  used ring
//   +0x360  header（virtio_blk_req，16B）
//   +0x370  status byte（1B）
//   +0x400  bounce buffer（block_size）
const QSZ: usize = 32; // 队列深度（2 的幂）
const AVAIL_OFF: usize = 0x200; // descriptor table 在页首（偏移 0），无需常量
const USED_OFF: usize = 0x250;
const HEADER_OFF: usize = 0x360;
const STATUS_OFF: usize = 0x370;
const BOUNCE_OFF: usize = 0x400;

const DESC_F_NEXT: u16 = 1; // 链式：desc.next 有效
const DESC_F_WRITE: u16 = 2; // 设备写（buffer 是 in）

const VIRTIO_BLK_T_IN: u32 = 0; // 读：设备写数据
const VIRTIO_BLK_T_OUT: u32 = 1; // 写：设备读数据

/// 描述符（16B）— `addr` 为 guest 物理地址（恒等 PA==VA）。
#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// available ring — 驱动提交已写好的描述符链。
#[repr(C)]
struct AvailRing {
    flags: u16,
    idx: u16,
    ring: [u16; QSZ],
    used_event: u16,
}

/// used ring 条目 — 设备回填完成项。
#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

/// used ring — 设备写完成的描述符链头。
#[repr(C)]
struct UsedRing {
    flags: u16,
    idx: u16,
    ring: [UsedElem; QSZ],
    avail_event: u16,
}

/// virtio_blk 请求头（16B）— 常驻队列页。
#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlkReq {
    ty: u32,
    reserved: u32,
    sector: u64,
}

// ── MMIO 辅助 ─────────────────────────────────────────────

/// 读取 32 位 MMIO 寄存器。
///
/// # Safety
///
/// 调用方须保证 `base` 指向已映射的 virtio-mmio 区域。
#[inline]
unsafe fn read32(base: *const u8, offset: usize) -> u32 {
    unsafe { (base.add(offset) as *const u32).read_volatile() }
}

/// 写入 32 位 MMIO 寄存器。
///
/// # Safety
///
/// 调用方须保证 `base` 指向已映射的 virtio-mmio 区域。
#[inline]
unsafe fn write32(base: *mut u8, offset: usize, val: u32) {
    unsafe { (base.add(offset) as *mut u32).write_volatile(val) }
}

// ── VirtQueue ─────────────────────────────────────────────

/// 单个 virtqueue — 常驻队列页内的平铺布局（单请求在途）。
pub(crate) struct VirtQueue {
    /// 队列页指针（恒等 PA==VA，frame 分配，永不释放）
    page: *mut u8,
    /// 已提交但未确认完成的 used idx（等完成用）
    last_used: u16,
    /// 请求头（队列页内常驻）
    header: *mut VirtioBlkReq,
    /// 完成状态字节（队列页内常驻；0 = OK）
    status: *mut u8,
    /// 数据 bounce buffer（队列页内常驻，block_size 字节）
    bounce: *mut u8,
}

/// 请求是否已完成：used idx 前进且头部为本请求（head=0）。
fn used_ready(page: *mut u8, last_used: u16) -> bool {
    // SAFETY: page 指向恒等队列页，布局已固定。
    let used = unsafe { &*(page.add(USED_OFF) as *const UsedRing) };
    let idx = used.idx;
    if idx == last_used {
        return false;
    }
    // 消费从 last_used 开始的下一项（单请求在途，head 恒 0）
    used.ring[(last_used as usize) % QSZ].id == 0
}

/// 提交并等待完成（队列锁内调用；SIE=0 → wait_event 轮询）。
fn submit(base: *mut u8, block_size: usize, ty: u32, block: u32, q: &mut VirtQueue) {
    // SAFETY: 队列页已分配且布局固定；MMIO 已映射。
    unsafe {
        let desc = q.page as *mut Desc;
        let avail = q.page.add(AVAIL_OFF) as *mut AvailRing;
        let req = q.header;
        let status = q.status;

        // 3 描述符链：header → bounce → status
        (*desc.add(0)) = Desc {
            addr: req as u64,
            len: 16,
            flags: DESC_F_NEXT,
            next: 1,
        };
        (*desc.add(1)) = Desc {
            addr: q.bounce as u64,
            len: block_size as u32,
            flags: if ty == VIRTIO_BLK_T_IN {
                DESC_F_NEXT | DESC_F_WRITE
            } else {
                DESC_F_NEXT
            },
            next: 2,
        };
        (*desc.add(2)) = Desc {
            addr: status as u64,
            len: 1,
            flags: DESC_F_WRITE,
            next: 0,
        };
        (*req) = VirtioBlkReq {
            ty,
            reserved: 0,
            sector: block as u64,
        };
        (*status) = 0;

        // available ring 提交（head = 0）
        let idx = (*avail).idx;
        (*avail).ring[(idx as usize) % QSZ] = 0;
        // avail 写后通知前 fence（设备可见描述符/avail）
        core::arch::asm!("fence", options(nostack, preserves_flags));
        (*avail).idx = idx.wrapping_add(1);
        // 通知设备
        write32(base, REG_QUEUE_NOTIFY, 0);
    }

    // 等完成：锁内 SIE=0 → wait_event 轮询 done()；未来无锁路径走 wfi 中断唤醒。
    // 闭包只捕获恒等页指针与 last_used（Copy），不借用 q。
    let page = q.page;
    let last_used = q.last_used;
    crate::schedule::wait_event(crate::schedule::Event::Block, || {
        used_ready(page, last_used)
    });

    // SAFETY: used_ready 已保证 used idx 前进且完成项为 head=0；读后 fence 推进。
    unsafe {
        core::arch::asm!("fence", options(nostack, preserves_flags));
        let used = q.page.add(USED_OFF) as *const UsedRing;
        q.last_used = (*used).idx;
        debug_assert_eq!((*q.status), 0, "virtio-blk request failed (status != 0)");
    }
}

// ── 设备实例 ──────────────────────────────────────────────

/// virtio-blk 实例 — MMIO + 队列（完成同步化）。
pub struct VirtioBlk {
    base: *mut u8,
    irq: u32,
    block_size: usize,
    block_count: usize,
    queue: SpinLock<VirtQueue>,
}

// SAFETY: 单 hart 内核；base 指向的 MMIO 与队列页（恒等 DMA）生命周期与内核等同。
unsafe impl Send for VirtioBlk {}
unsafe impl Sync for VirtioBlk {}

impl crate::hal::block::BlockDevice for VirtioBlk {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> usize {
        self.block_count
    }

    fn read_block(&self, block: u32, buf: &mut [u8]) {
        debug_assert!(
            (block as usize) < self.block_count,
            "virtio: block out of range"
        );
        let base = self.base;
        let bs = self.block_size;
        let mut q = self.queue.lock(); // 关中断 → 轮询完成
        submit(base, bs, VIRTIO_BLK_T_IN, block, &mut q);
        // SAFETY: bounce 缓冲区 ≥ block_size，buf 由调用方保证 ≥ block_size。
        unsafe { core::ptr::copy_nonoverlapping(q.bounce, buf.as_mut_ptr(), bs.min(buf.len())) };
    }

    fn write_block(&self, block: u32, buf: &[u8]) {
        debug_assert!(
            (block as usize) < self.block_count,
            "virtio: block out of range"
        );
        let base = self.base;
        let bs = self.block_size;
        let mut q = self.queue.lock(); // 关中断 → 轮询完成
        // SAFETY: bounce 缓冲区 ≥ block_size，buf 由调用方保证 ≥ block_size。
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), q.bounce, bs.min(buf.len())) };
        submit(base, bs, VIRTIO_BLK_T_OUT, block, &mut q);
    }

    fn interrupt_number(&self) -> u32 {
        self.irq
    }

    fn ack_interrupt(&self) {
        // SAFETY: MMIO 已映射；读中断状态并按位回写 ACK（写 1 清位）。
        unsafe {
            let status = read32(self.base, REG_INTERRUPT_STATUS);
            write32(self.base, REG_INTERRUPT_ACK, status);
        }
    }
}

// ── 驱动 ─────────────────────────────────────────────────

/// virtio-blk 驱动。
pub struct VirtioBlkDriver;

impl Driver for VirtioBlkDriver {
    fn name(&self) -> &'static str {
        "virtio-blk"
    }

    fn compatibles(&self) -> &'static [&'static str] {
        &["virtio,mmio"]
    }

    fn probe(&self, dev: &Device) -> Result<(), DriverError> {
        // 依赖检查前置：PLIC 未 probe 时直接返回 Deferred，不产生任何副作用
        // （deferred 重试会再次调用本 probe，副作用必须发生在依赖就绪之后）。
        let plic = hub::find::<Plic>().ok_or(DriverError::Deferred)?;
        let irq = dev
            .interrupt
            .ok_or(DriverError::Init("virtio-blk: no interrupt in DTB"))?;

        // MMIO 映射（driver::map_mmio：取整 + 内核空间映射）
        unsafe { crate::driver::map_mmio(dev) }?;
        let base = dev.base.as_usize() as *mut u8;

        // 校验 magic / version / device id
        // SAFETY: map_mmio 已建立恒等映射，MMIO 区域可访问。
        let magic = unsafe { read32(base, REG_MAGIC) };
        let version = unsafe { read32(base, REG_VERSION) };
        let device_id = unsafe { read32(base, REG_DEVICE_ID) };
        if magic != 0x7472_6976 {
            return Err(DriverError::Init("virtio-blk: bad magic value"));
        }
        if version != 1 && version != 2 {
            return Err(DriverError::Init(
                "virtio-blk: unrecognized transport version",
            ));
        }
        if device_id != VIRTIO_ID_BLOCK {
            // 非块 virtio 设备（空槽/console）：不绑定不注册，返回 Ok 让 hub 记
            // Bound（该设备被本驱动认领但跳过），避免 Stuck 中断整个 probe。
            debug!("virtio: device_id {device_id} != block(2), skipping");
            return Ok(());
        }

        // 特性协商：复位 → ACKNOWLEDGE → DRIVER → 读特性（VERSION_1）→ FEATURES_OK
        //
        // 要求 VERSION_1（bit 32）：QEMU virtio-mmio 默认 force-legacy=on（legacy
        // 模式，不提供 VERSION_1 且忽略现代队列地址寄存器），须 `-global
        // virtio-mmio.force-legacy=off` 强制 modern——现代模式才提供 VERSION_1，
        // 驱动协商它进入现代布局。
        // SAFETY: 见上。
        unsafe {
            write32(base, REG_STATUS, 0); // 设备复位
            write32(base, REG_STATUS, STATUS_ACKNOWLEDGE);
            write32(base, REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
            // VIRTIO_F_VERSION_1 = bit 32：DeviceFeaturesSel=1 → 读高 32，bit0 = 1<<32
            write32(base, REG_DEVICE_FEATURES_SEL, 1);
            let features_hi = read32(base, REG_DEVICE_FEATURES);
            if features_hi & 1 == 0 {
                return Err(DriverError::Init(
                    "virtio-blk: no VIRTIO_F_VERSION_1 (add -global virtio-mmio.force-legacy=off)",
                ));
            }
            // 只协商 VERSION_1（其余特性不支持，不声明）
            write32(base, REG_DRIVER_FEATURES_SEL, 1);
            write32(base, REG_DRIVER_FEATURES, 1);
            // FEATURES_OK 校验
            write32(
                base,
                REG_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
            );
            let status = read32(base, REG_STATUS);
            if status & STATUS_FEATURES_OK == 0 {
                return Err(DriverError::Init("virtio-blk: FEATURES_OK rejected"));
            }
        }

        // 分配一页恒等 DMA 内存，清零后平铺 virtqueue + bounce
        let layout = Layout::from_size_align(crate::memory::PAGE_SIZE, crate::memory::PAGE_SIZE)
            .expect("virtio-blk: invalid layout");
        let page = frame::allocator()
            .allocate(layout)
            .map_err(|_| DriverError::Init("virtio-blk: no frame for virtqueue"))?;
        let page_ptr = page.as_ptr() as *mut u8;
        // SAFETY: 刚分配的物理帧（页对齐、长度 ≥ 一页），DRAM 恒等映射区可直写。
        unsafe { core::ptr::write_bytes(page_ptr, 0, crate::memory::PAGE_SIZE) };

        // 队列激活：QueueSel=0 → QueueNum=QSZ → 写队列地址 → QueueReady → DRIVER_OK
        // SAFETY: MMIO 已映射；队列页为恒等 PA==VA。
        unsafe {
            write32(base, REG_QUEUE_SEL, 0);
            let num_max = read32(base, REG_QUEUE_NUM_MAX);
            if (QSZ as u32) > num_max {
                return Err(DriverError::Init("virtio-blk: queue size exceeds num_max"));
            }
            write32(base, REG_QUEUE_NUM, QSZ as u32);
            let gpa = page_ptr as usize as u64;
            write32(base, REG_QUEUE_DESC_LOW, gpa as u32);
            write32(base, REG_QUEUE_DESC_HIGH, (gpa >> 32) as u32);
            let avail = gpa + AVAIL_OFF as u64;
            write32(base, REG_QUEUE_DRIVER_LOW, avail as u32);
            write32(base, REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
            let used = gpa + USED_OFF as u64;
            write32(base, REG_QUEUE_DEVICE_LOW, used as u32);
            write32(base, REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
            write32(base, REG_QUEUE_READY, 1);
            write32(
                base,
                REG_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
            );
        }

        // 块大小/容量：capacity（u64，512B 扇区）+ blk_size（u32，可能为 0 → 默认 512）
        // SAFETY: 配置区在 MMIO 内（0x100 起）。
        let capacity = unsafe { (base.add(CONFIG_CAPACITY) as *const u64).read_volatile() };
        let blk_size = unsafe { (base.add(CONFIG_BLK_SIZE) as *const u32).read_volatile() };
        let block_size = if blk_size > 0 { blk_size as usize } else { 512 };
        let block_count = (capacity * 512 / block_size as u64) as usize;

        // 构造实例 + 挂载到设备（Linux dev_set_drvdata 语义）
        let blk = Box::leak(Box::new(VirtioBlk {
            base,
            irq,
            block_size,
            block_count,
            queue: SpinLock::new(VirtQueue {
                page: page_ptr,
                last_used: 0,
                // SAFETY: 页已分配（≥ PAGE_SIZE），各偏移布局在页内（见 virtqueue 布局）。
                header: unsafe { page_ptr.add(HEADER_OFF) as *mut VirtioBlkReq },
                status: unsafe { page_ptr.add(STATUS_OFF) },
                bounce: unsafe { page_ptr.add(BOUNCE_OFF) },
            }),
        }));
        dev.set_instance(blk);
        info!(
            "virtio-blk: {} blocks × {} B ({} MiB)",
            block_count,
            block_size,
            block_count * block_size / (1024 * 1024)
        );

        // 块设备服务注册（hal 单例 + /dev/block0 + BlockIrqHandler）
        crate::file::io::block::register(blk);

        // 中断路由
        plic.set_priority(irq, 1);
        plic.enable(irq);

        Ok(())
    }
}

/// 驱动静态实例（block::DRIVERS 引用）。
pub static DRIVER: &dyn Driver = &VirtioBlkDriver;
