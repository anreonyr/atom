// SpinLock — 中断安全自旋锁，多核基础
//
// lock() 返回 SpinLockGuard，通过 Deref/DerefMut 提供 &T/&mut T 访问。
// 获取锁时关闭 S-mode 全局中断（sstatus.SIE），guard 析构时释放锁并恢复中断。
// 这解决了"任务上下文持锁 → 中断抢占 → 中断处理路径争同一把锁"的死锁。
//
// Drop guard 模式保证即使 unwind（若启用）,锁也能在 guard 析构时正确释放。

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// 同一时刻只有一个 SpinLockGuard 持有 &mut T，
// 且 guard 不实现 Send（锁必须在本 hart 上释放）。
unsafe impl<T> Sync for SpinLock<T> {}

/// 锁守卫 — 持有锁期间通过 Deref/DerefMut 访问受保护数据。
///
/// 析构时自动释放锁并恢复中断使能状态。
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    sie_was_enabled: bool,
}

impl<T> SpinLock<T> {
    pub const fn new(val: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }

    /// 获取锁，返回守卫。
    ///
    /// 获取前关闭 S-mode 全局中断（sstatus.SIE=0），
    /// 防止同 CPU 中断上下文重入导致死锁。
    /// 守卫析构时释放锁并恢复 SIE。
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // 保存当前 SIE 并关中断
        let sie_was_enabled = unsafe {
            let s = crate::hal::csr::sstatus::read();
            let was = s.contains(crate::hal::csr::sstatus::Sstatus::SIE);
            if was {
                crate::hal::csr::sstatus::clear(crate::hal::csr::sstatus::Sstatus::SIE);
            }
            was
        };

        // Acquire：保证后续读取看到之前写入的完整状态
        while self.locked.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }

        SpinLockGuard {
            lock: self,
            sie_was_enabled,
        }
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: SpinLock 保证同一时刻只有一个 guard 存在，
        // 且 guard 的 &self 对应唯一的 &T。
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: SpinLock 保证同一时刻只有一个 guard 存在，
        // 且 guard 的 &mut self 对应唯一的 &mut T。
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        // Release：保证之前写入在解锁时对其他核可见
        self.lock.locked.store(false, Ordering::Release);

        // 恢复 SIE
        if self.sie_was_enabled {
            unsafe {
                crate::hal::csr::sstatus::set(crate::hal::csr::sstatus::Sstatus::SIE);
            }
        }
    }
}

