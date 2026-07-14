// 内核内存分配子系统

pub mod bump;
pub mod frame;
pub mod page;

/// 初始化内存子系统（物理帧分配器）。
///
/// # Safety
///
/// 必须在 `main` 早期调用**恰好一次**，在任何堆分配之前。
/// 调用时 MMU 尚未启用，使用裸物理地址。
pub fn init() {
    unsafe {
        bump::init();
        page::init();
    }
}
