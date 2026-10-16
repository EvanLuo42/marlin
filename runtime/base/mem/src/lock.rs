//! A minimal futex lock for the allocator's rare global paths.
//!
//! marlin-sync is not used: it sits beside this crate and may itself allocate.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_pal::futex;

const UNLOCKED: u32 = 0;
const LOCKED: u32 = 1;
/// Locked, and someone may be waiting.
const CONTENDED: u32 = 2;

pub(crate) struct Lock(AtomicU32);

impl Lock {
    pub(crate) const fn new() -> Self {
        Self(AtomicU32::new(UNLOCKED))
    }

    #[inline]
    pub(crate) fn lock(&self) {
        if self
            .0
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_contended();
        }
    }

    #[cold]
    fn lock_contended(&self) {
        for _ in 0..100 {
            if self.0.load(Ordering::Relaxed) == UNLOCKED
                && self
                    .0
                    .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return;
            }
            spin_loop();
        }
        while self.0.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
            futex::wait(&self.0, CONTENDED, None);
        }
    }

    #[inline]
    pub(crate) fn unlock(&self) {
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

// SAFETY: the value is only reached through `with`, which holds the lock. The allocator's values
// are raw pointers to memory it manages, which any thread may use under the lock.
unsafe impl<T> Sync for Locked<T> {}

impl<T> Locked<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            lock: Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.lock.lock();
        // SAFETY: the lock gives exclusive access. A panic in `f` aborts the process, so the lock
        // is never left held.
        let result = f(unsafe { &mut *self.value.get() });
        self.lock.unlock();
        result
    }
}
