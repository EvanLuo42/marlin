use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::time::Duration;

use marlin_pal::futex;

use crate::SPIN_LIMIT;
use crate::timeout::{self, Timeout};

const UNLOCKED: u32 = 0;
/// Locked, and no thread is (known to be) asleep waiting for it.
const LOCKED: u32 = 1;
/// Locked, and threads may be asleep; unlocking must wake one.
const CONTENDED: u32 = 2;

/// A mutual-exclusion lock without data: one 32-bit word, for building other primitives on.
///
/// Uncontended lock and unlock are one atomic each. A contended `lock` spins briefly while the
/// holder runs, then sleeps on the word.
pub struct RawMutex {
    state: AtomicU32,
}

impl RawMutex {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(UNLOCKED),
        }
    }

    #[inline]
    pub fn lock(&self) {
        if self
            .state
            .compare_exchange(UNLOCKED, LOCKED, Acquire, Relaxed)
            .is_err()
        {
            self.lock_contended(None);
        }
    }

    #[inline]
    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED, Acquire, Relaxed)
            .is_ok()
    }

    /// Like [`lock`](Self::lock), giving up after `timeout`; returns `true` if the lock was
    /// acquired. A zero timeout still spins briefly before giving up.
    #[inline]
    pub fn try_lock_for(&self, timeout: Duration) -> bool {
        self.try_lock() || self.lock_contended(Some(&Timeout::start(timeout)))
    }

    /// Releases the lock.
    ///
    /// # Safety
    ///
    /// The lock must be held, and the caller must be the one entitled to release it (whoever
    /// acquired it, or whoever that acquirer handed it to).
    #[inline]
    pub unsafe fn unlock(&self) {
        if self.state.swap(UNLOCKED, Release) == CONTENDED {
            futex::wake_one(&self.state);
        }
    }

    /// Whether some thread holds the lock. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.state.load(Relaxed) != UNLOCKED
    }

    /// Returns `false` on timeout.
    #[cold]
    fn lock_contended(&self, timeout: Option<&Timeout>) -> bool {
        let mut state = self.spin();

        // Still uncontended: take it without announcing a sleeper, so unlock skips the wake.
        if state == UNLOCKED {
            match self
                .state
                .compare_exchange(UNLOCKED, LOCKED, Acquire, Relaxed)
            {
                Ok(_) => return true,
                Err(s) => state = s,
            }
        }

        loop {
            // Mark CONTENDED before sleeping. If the swap found it unlocked we now own it, and
            // still leave it CONTENDED: other sleepers may exist and need the wake on unlock.
            if state != CONTENDED && self.state.swap(CONTENDED, Acquire) == UNLOCKED {
                return true;
            }
            // Give up only here, with the word CONTENDED while someone holds the lock: if the
            // last unlock's wake reached us, the holder's unlock will wake another sleeper.
            let Some(remaining) = timeout::wait_time(timeout) else {
                return false;
            };
            futex::wait(&self.state, CONTENDED, remaining);
            state = self.spin();
        }
    }

    /// Spins while the lock is held by a running thread. Stops at once if there are sleepers:
    /// then the queue is long and spinning only delays our own sleep.
    fn spin(&self) -> u32 {
        let mut spins = SPIN_LIMIT;
        loop {
            let state = self.state.load(Relaxed);
            if state != LOCKED || spins == 0 {
                return state;
            }
            core::hint::spin_loop();
            spins -= 1;
        }
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for RawMutex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawMutex")
            .field("locked", &self.is_locked())
            .finish()
    }
}

/// A mutual-exclusion lock protecting a `T`. See [`RawMutex`] for the algorithm.
pub struct Mutex<T: ?Sized> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

// SAFETY: the lock hands out access to `T` to one thread at a time, so it is enough that `T` can
// be sent between threads.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawMutex::new(),
            data: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Blocks until the lock is acquired. Locking a mutex the calling thread already holds
    /// deadlocks.
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard::new(self)
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw.try_lock().then(|| MutexGuard::new(self))
    }

    /// Like [`lock`](Self::lock), giving up after `timeout`.
    #[inline]
    pub fn try_lock_for(&self, timeout: Duration) -> Option<MutexGuard<'_, T>> {
        self.raw
            .try_lock_for(timeout)
            .then(|| MutexGuard::new(self))
    }

    /// Whether the lock is held. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    /// Access without locking: `&mut self` proves no guard exists.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Mutex");
        match self.try_lock() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &format_args!("<locked>")),
        };
        d.finish_non_exhaustive()
    }
}

/// Holds a [`Mutex`] locked; unlocks on drop.
#[must_use = "dropping the guard unlocks the mutex immediately"]
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    // Makes the auto traits those of `&mut T`: Send if `T: Send`, Sync only if `T: Sync`.
    _marker: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            _marker: PhantomData,
        }
    }

    pub(crate) fn raw(&self) -> &'a RawMutex {
        &self.mutex.raw
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock, so no other reference to the data is live.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: the guard was created by acquiring the lock and is dropped exactly once.
        unsafe { self.mutex.raw.unlock() }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
