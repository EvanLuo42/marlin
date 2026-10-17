//! A minimal futex lock for the sinks' buffers.
//!
//! marlin-sync is not used: it sits beside this crate.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_pal::futex;

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
/// Locked, and someone may be waiting.
const CONTENDED: u32 = 2;

struct Lock(AtomicU32);

impl Lock {
    const fn new() -> Self {
        Self(AtomicU32::new(UNLOCKED))
    }

    #[inline]
    fn try_lock(&self) -> bool {
        self.0
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    fn lock(&self) {
        if !self.try_lock() {
            self.lock_contended();
        }
    }

    #[cold]
    fn lock_contended(&self) {
        for _ in 0..100 {
            if self.0.load(Ordering::Relaxed) == UNLOCKED && self.try_lock() {
                return;
            }
            spin_loop();
        }
        while self.0.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
            futex::wait(&self.0, CONTENDED, None);
        }
    }

    #[inline]
    fn unlock(&self) {
        if self.0.swap(UNLOCKED, Ordering::Release) == CONTENDED {
            futex::wake_one(&self.0);
        }
    }
}

/// A value only reachable while holding its lock.
pub(crate) struct Locked<T> {
    lock: Lock,
    value: UnsafeCell<T>,
}

// SAFETY: the value is only reached through `with`/`try_with`, which hold the lock, so it moves
// between threads but is never shared.
unsafe impl<T: Send> Sync for Locked<T> {}

impl<T> Locked<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            lock: Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.lock.lock();
        // SAFETY: the lock gives exclusive access. The closures in this crate do not panic, so
        // the lock is never left held.
        let result = f(unsafe { &mut *self.value.get() });
        self.lock.unlock();
        result
    }

    /// Like [`with`](Self::with), but gives up after a short spin instead of blocking. For
    /// readers that may run in a panic handler.
    pub(crate) fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        for _ in 0..1000 {
            if self.lock.try_lock() {
                // SAFETY: as in `with`.
                let result = f(unsafe { &mut *self.value.get() });
                self.lock.unlock();
                return Some(result);
            }
            spin_loop();
        }
        None
    }

    pub(crate) fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}
