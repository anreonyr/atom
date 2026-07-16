// RwLock — 中断安全读写锁，多读单写
//
// 用单个 AtomicUsize 编码状态：最高位 WRITER_BIT 表示写者持有，
// 低位记录活跃读者数。多个读者可并发持有，写者独占。
// 读写获取都关中断（复用 TrapGuard），可从中断上下文安全获取。
//
// 写者优先策略：一旦写者置位 WRITER_BIT，新读者无法再获取，防止写者饿死。
// guard 携带 !Send 标记，保证在本 hart 释放。

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::trap::TrapGuard;

// 最高位：写者持有标志
const WRITER_BIT: usize = 1 << (usize::BITS - 1);
// 低位掩码：读者计数
const READER_MASK: usize = !WRITER_BIT;

pub struct RwLock<T: ?Sized> {
    // WRITER_BIT | reader_count
    state: AtomicUsize,
    data: UnsafeCell<T>,
}

// SAFETY: 读者需要 T: Sync（多个 &T 跨 hart），写者需要 T: Send（&mut T 转移）。
// guard !Send，锁在本 hart 释放。
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

/// 读守卫 — 持有期间通过 Deref 访问 &T，析构时释放读锁。
pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    _not_send: PhantomData<*const ()>,
    _trap: TrapGuard,
}

/// 写守卫 — 持有期间通过 Deref/DerefMut 访问 &mut T，析构时释放写锁。
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    _not_send: PhantomData<*const ()>,
    _trap: TrapGuard,
}

impl<T> RwLock<T> {
    pub const fn new(val: T) -> Self {
        RwLock {
            state: AtomicUsize::new(0),
            data: UnsafeCell::new(val),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// 获取读锁，返回读守卫。多个读者可并发持有。
    ///
    /// 若已有写者持有或等待，则自旋等待。获取期间关中断。
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        // SAFETY: 处于 S-mode；关中断防止本 hart 中断重入。
        let trap = unsafe { TrapGuard::save() };

        loop {
            // Acquire：读者进入临界区前看到写者的所有写入
            let s = self.state.fetch_add(1, Ordering::Acquire);
            if s & WRITER_BIT == 0 {
                // 无写者，读锁获取成功
                break;
            }
            // 有写者：撤销本次读者计数，自旋后重试
            self.state.fetch_sub(1, Ordering::Release);
            while self.state.load(Ordering::Relaxed) & WRITER_BIT != 0 {
                core::hint::spin_loop();
            }
        }

        RwLockReadGuard {
            lock: self,
            _not_send: PhantomData,
            _trap: trap,
        }
    }

    /// 获取写锁，返回写守卫。写者独占。
    ///
    /// 先置 WRITER_BIT 阻塞新读者，再等待现存读者归零。获取期间关中断。
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        // SAFETY: 处于 S-mode；关中断防止本 hart 中断重入。
        let trap = unsafe { TrapGuard::save() };

        // 抢占 WRITER_BIT：从"无写者"状态置位
        loop {
            let s = self.state.load(Ordering::Relaxed);
            if s & WRITER_BIT == 0
                && self
                    .state
                    .compare_exchange(s, s | WRITER_BIT, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                break;
            }
            core::hint::spin_loop();
        }

        // 等待现存读者全部离开
        while self.state.load(Ordering::Acquire) & READER_MASK != 0 {
            core::hint::spin_loop();
        }

        RwLockWriteGuard {
            lock: self,
            _not_send: PhantomData,
            _trap: trap,
        }
    }
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: 持有读锁期间无写者，可安全共享 &T。
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // Release：读者退出前的读取对后续写者可见
        self.lock.state.fetch_sub(1, Ordering::Release);
    }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: 写锁独占，无其他访问者。
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: 写锁独占，&mut self 对应唯一 &mut T。
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Release：清除 WRITER_BIT（读者计数此刻为 0），写入对后续获取者可见
        self.lock.state.store(0, Ordering::Release);
    }
}
