use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// A lock that never sleeps: a contended `lock` spins (test-and-test-and-set) until the holder
/// lets go.
///
/// Use it only where the critical section is a handful of instructions and sleeping is wrong or
/// impossible: code that must not enter the kernel, or a section so short that a futex round
/// trip would cost more than the wait. Never hold it across anything that can block or take
/// long (I/O, allocation, another lock that sleeps), and prefer [`Mutex`](crate::Mutex) otherwise:
/// if the holder is preempted, every waiter burns its whole time slice, and nothing stops a
/// waiter of higher priority from starving the holder off its core.
pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: the lock hands out access to `T` to one thread at a time, as in `Mutex`.
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Spins until the lock is acquired. Locking a spin lock the calling thread already holds
    /// spins forever.
    #[inline]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        while self.locked.swap(true, Acquire) {
            // Wait on a plain load, which keeps the cache line shared, rather than hammering it
            // with swaps.
            while self.locked.load(Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinLockGuard::new(self)
    }

    #[inline]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        (!self.locked.load(Relaxed) && !self.locked.swap(true, Acquire))
            .then(|| SpinLockGuard::new(self))
    }

    /// Whether the lock is held. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.locked.load(Relaxed)
    }

    /// Access without locking: `&mut self` proves no guard exists.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for SpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for SpinLock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for SpinLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("SpinLock");
        match self.try_lock() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &format_args!("<locked>")),
        };
        d.finish_non_exhaustive()
    }
}

/// Holds a [`SpinLock`] locked; unlocks on drop.
#[must_use = "dropping the guard unlocks the lock immediately"]
pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    // Auto traits of `&mut T`, as for `MutexGuard`.
    _marker: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> SpinLockGuard<'a, T> {
    fn new(lock: &'a SpinLock<T>) -> Self {
        Self {
            lock,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock, so no other reference to the data is live.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.locked.store(false, Release);
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for SpinLockGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for SpinLockGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
