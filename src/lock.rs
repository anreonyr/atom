// SpinLock — 自旋锁，多核基础
//
// 在单核环境中立即通过（swap + 读会看到 false），无实际等待；
// 多核时通过 Acquire/Release 语义保证互斥访问。

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// 只要 accessor 返回的是 &mut T（lock 的闭包持有），Sync 就安全——
// 闭包外不可能有别的 &T 引用。
unsafe impl<T> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(val: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }

    /// 获取锁，在闭包内提供 `&mut T`
    pub fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        // Acquire：保证后续读取看到之前写入的完整状态
        while self.locked.swap(true, Ordering::Acquire) {
            // PAUSE hint，减少总线竞争
            core::hint::spin_loop();
        }
        let result = f(unsafe { &mut *self.data.get() });
        // Release：保证之前写入在解锁时对其他核可见
        self.locked.store(false, Ordering::Release);
        result
    }
}
