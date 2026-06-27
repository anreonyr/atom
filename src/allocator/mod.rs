// 内核内存分配子系统
//
// # 架构
//
// ```text
// ┌─────────────────────────────────────────────────┐
// │         hybrid::HybridAllocator                 │
// │         #[global_allocator]                     │
// ├─────────────────────────────────────────────────┤
// │  size ≤ 512 && align ≤ 8  →  Slab (对象缓存)    │
// │  其他                       →  Buddy (幂次分配)  │
// ├─────────────────────────────────────────────────┤
// │  slab::SLAB (SpinLock)   buddy::BUDDY (SpinLock)│
// │  缓存: 32/64/128/256/512B   阶数: 4..16         │
// │  独立页池 (.bss)            64 KiB 堆 (.bss)     │
// └─────────────────────────────────────────────────┘
// ```
//
// # 模块
//
// | 模块       | 职责                                       |
// |-----------|-------------------------------------------|
// | `hybrid`  | HybridAllocator + #[global_allocator]      |
// | `buddy`   | Buddy 幂次分配器 — 管理 64 KiB 堆           |
// | `slab`    | Slab 对象缓存 — 小对象 O(1) 分配            |
// | `frame`   | 物理帧分配器 — BitmapAllocator (8 MiB DRAM)  |

pub mod buddy;
pub mod frame;
pub mod hybrid;
pub mod slab;


/// 初始化内存子系统（堆分配器 + 物理帧分配器）。
///
/// # Safety
///
/// 必须在 `main` 早期调用**恰好一次**，在任何堆分配之前。
/// 调用时 MMU 尚未启用，使用裸物理地址。
pub fn init() {
    unsafe {
        buddy::init();   // 堆分配器（buddy + slab）
        frame::init();   // 物理帧分配器（位图）
    }
}
