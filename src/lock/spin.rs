// SpinLock — 中断安全自旋锁，多核基础
//
// lock() 返回 SpinLockGuard，通过 Deref/DerefMut 提供 &T/&mut T 访问。
// 获取锁时关闭 S-mode 全局中断（sstatus.SIE），guard 析构时释放锁并恢复中断。
// 这解决了"任务上下文持锁 → 中断抢占 → 中断处理路径争同一把锁"的死锁。
//
// 关中断逻辑委托给 TrapGuard（见 lock/trap.rs）；guard 携带 !Send 标记，
// 保证锁必须在获取它的同一 hart 上释放（多核安全前提）。

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use super::trap::TrapGuard;

pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: 同一时刻只有一个 SpinLockGuard 持有 &mut T，
// 且 guard 不实现 Send（锁必须在本 hart 上释放）。
// 无条件 Sync：内核需在跨上下文场景保护含裸指针的状态（如 MMIO、TrapFrame*）。
unsafe impl<T: ?Sized> Sync for SpinLock<T> {}

/// 锁守卫 — 持有锁期间通过 Deref/DerefMut 访问受保护数据。
///
/// 析构时自动释放锁并恢复中断使能状态。
pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    // *const () 既不 Send 也不 Sync：强制 guard 在本 hart 上释放
    _not_send: PhantomData<*const ()>,
    // 持有期间关中断，其 Drop 在 guard Drop 之后执行以恢复 SIE
    _trap: TrapGuard,
}

impl<T> SpinLock<T> {
    pub const fn new(val: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// 获取锁，返回守卫。
    ///
    /// 获取前关闭 S-mode 全局中断（sstatus.SIE=0），
    /// 防止同 CPU 中断上下文重入导致死锁。
    /// 守卫析构时释放锁并恢复 SIE。
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // SAFETY: 处于 S-mode；关中断防止本 hart 中断重入。
        let trap = unsafe { TrapGuard::save() };

        // Acquire：保证后续读取看到之前写入的完整状态
        while self.locked.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }

        #[cfg(feature = "spin-trace")]
        crate::lock_debug!(
            "spinlock lock @ {:#x}",
            self as *const Self as *const () as usize
        );

        SpinLockGuard {
            lock: self,
            _not_send: PhantomData,
            _trap: trap,
        }
    }

    /// 尝试获取锁，成功返回守卫，失败返回 `None`（不自旋）。
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        // SAFETY: 处于 S-mode；关中断防止本 hart 中断重入。
        let trap = unsafe { TrapGuard::save() };

        // 获取失败时 trap 析构自动恢复中断
        if self.locked.swap(true, Ordering::Acquire) {
            return None;
        }

        #[cfg(feature = "spin-trace")]
        crate::lock_debug!(
            "spinlock try_lock @ {:#x}",
            self as *const Self as *const () as usize
        );

        Some(SpinLockGuard {
            lock: self,
            _not_send: PhantomData,
            _trap: trap,
        })
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: SpinLock 保证同一时刻只有一个 guard 存在，
        // 且 guard 的 &self 对应唯一的 &T。
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: SpinLock 保证同一时刻只有一个 guard 存在，
        // 且 guard 的 &mut self 对应唯一的 &mut T。
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(feature = "spin-trace")]
        crate::lock_debug!(
            "spinlock unlock @ {:#x}",
            self.lock as *const SpinLock<T> as *const () as usize
        );
        // Release：保证之前写入在解锁时对其他核可见
        self.lock.locked.store(false, Ordering::Release);
        // _trap 字段随后析构，恢复 SIE
    }
}
