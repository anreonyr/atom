// 内核分配器门户 — 通过 trait object 在不同启动阶段切换分配器实现
//
// PortalAllocator 作为 #[global_allocator]，内部持有 &dyn Allocator，
// 可在不同启动阶段委托给不同的分配器实例：
//   1. 初始阶段：None，任何分配返回 Err
//   2. 早期启动：委托给 bump 分配器
//   3. 运行时：委托给 buddy 分配器
//
// 各后端分配器通过全局 static 提供 'static 引用，无需额外包装函数。

use core::alloc::Layout;
use core::ptr::NonNull;

use alloc::alloc::{AllocError, Allocator, GlobalAllocator};

use crate::lock::SpinLock;

/// 门户分配器 — 通过 trait object 委托给实际分配器。
pub struct PortalAllocator {
    inner: SpinLock<PortalInner>,
}

/// 门户分配器内部状态 — 由 SpinLock 保护。
struct PortalInner {
    allocator: Option<&'static dyn Allocator>,
}

impl PortalAllocator {
    /// 创建门户分配器，初始化为空。
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(PortalInner { allocator: None }),
        }
    }
}

// SAFETY: PortalAllocator 通过 SpinLock 保护内部指针，同一时刻只有一个上下文修改或读取。
unsafe impl Sync for PortalAllocator {}

unsafe impl GlobalAllocator for PortalAllocator {}

unsafe impl Allocator for PortalAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let inner = self.inner.lock();
        inner.allocator.ok_or(AllocError)?.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let inner = self.inner.lock();
        if let Some(allocator) = inner.allocator {
            // SAFETY: 调用者的安全义务传递给后端分配器。
            allocator.deallocate(ptr, layout);
        }
    }
}

/// 切换到指定的分配器实例。
///
/// 调用者负责确保目标分配器已完全初始化。
/// 从 bump 切换到 buddy 后，bump 中已分配的内存不会被回收，
/// 但新的分配请求将走 buddy 路径。
pub fn switch(allocator: &'static dyn Allocator) {
    let mut inner = PORTAL_ALLOCATOR.inner.lock();
    inner.allocator = Some(allocator);
}
/// 全局门户分配器实例 — 内核唯一的 #[global_allocator]。
///
/// 初始状态为空，在 `allocator::init()` 中切换到 bump 分配器。
#[global_allocator]
pub static PORTAL_ALLOCATOR: PortalAllocator = PortalAllocator::new();
