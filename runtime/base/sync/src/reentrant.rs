use core::cell::{Cell, UnsafeCell};
use core::fmt;
use core::marker::PhantomData;
use core::ops::Deref;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::Relaxed;
use core::time::Duration;

use marlin_pal::thread;

use crate::RawMutex;

/// No thread: OS thread ids are never 0 on any supported platform.
const NO_OWNER: u64 = 0;

/// A mutex the holding thread may lock again without deadlocking. Guards give only `&T`, since
/// several of them can be live on the owning thread at once; put a `Cell` or `RefCell` inside
/// for mutation.
///
/// A [`RawMutex`] plus the owner's OS thread id and a recursion count. Every lock call asks the
/// PAL for the current thread id, which is a syscall on Linux (`gettid`), so this is slower than
/// [`Mutex`](crate::Mutex) even uncontended.
///
/// Not fiber-safe: ownership is tracked per OS thread, so a fiber that migrates to another
/// thread while holding the lock breaks it (the new thread is not the owner, and the old thread
/// is, until the count drops to zero). Fiber code wants the fiber-aware locks in `marlin-jobs`.
pub struct ReentrantMutex<T: ?Sized> {
    raw: RawMutex,
    owner: AtomicU64,
    // Only touched by the owning thread, while it holds `raw`.
    count: Cell<u32>,
    data: UnsafeCell<T>,
}

// SAFETY: moving the lock moves the `T`.
unsafe impl<T: ?Sized + Send> Send for ReentrantMutex<T> {}
// SAFETY: only the owning thread reaches `T` (guards are not `Send`), one thread at a time, as in
// `Mutex`. `count` is only touched by the owner, under `raw`.
unsafe impl<T: ?Sized + Send> Sync for ReentrantMutex<T> {}

impl<T> ReentrantMutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawMutex::new(),
            owner: AtomicU64::new(NO_OWNER),
            count: Cell::new(0),
            data: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> ReentrantMutex<T> {
    /// Blocks until the lock is acquired, or returns at once if this thread already holds it.
    pub fn lock(&self) -> ReentrantMutexGuard<'_, T> {
        let id = thread::current_id();
        if !self.relock(id) {
            self.raw.lock();
            self.take(id);
        }
        ReentrantMutexGuard::new(self)
    }

    pub fn try_lock(&self) -> Option<ReentrantMutexGuard<'_, T>> {
        let id = thread::current_id();
        if !self.relock(id) {
            if !self.raw.try_lock() {
                return None;
            }
            self.take(id);
        }
        Some(ReentrantMutexGuard::new(self))
    }

    /// Like [`lock`](Self::lock), giving up after `timeout`.
    pub fn try_lock_for(&self, timeout: Duration) -> Option<ReentrantMutexGuard<'_, T>> {
        let id = thread::current_id();
        if !self.relock(id) {
            if !self.raw.try_lock_for(timeout) {
                return None;
            }
            self.take(id);
        }
        Some(ReentrantMutexGuard::new(self))
    }

    /// Whether some thread holds the lock. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    /// Access without locking: `&mut self` proves no guard exists.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    /// Bumps the count if thread `id` already holds the lock.
    fn relock(&self, id: u64) -> bool {
        // Relaxed suffices: only this thread ever stores `id` here, so reading it means this
        // thread stored it and has not cleared it since; any other value means it does not own
        // the lock, however stale the read.
        if self.owner.load(Relaxed) != id {
            return false;
        }
        let count = self.count.get().checked_add(1);
        self.count
            .set(count.expect("ReentrantMutex lock count overflow"));
        true
    }

    /// Records thread `id` as the owner after acquiring `raw`.
    fn take(&self, id: u64) {
        self.owner.store(id, Relaxed);
        self.count.set(1);
    }
}

impl<T: Default> Default for ReentrantMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for ReentrantMutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for ReentrantMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("ReentrantMutex");
        match self.try_lock() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &format_args!("<locked>")),
        };
        d.finish_non_exhaustive()
    }
}

/// Holds a [`ReentrantMutex`] locked (once); unlocks on drop of the last guard on the thread.
///
/// Unlike the other guards this one is not `Send`: the lock belongs to a thread, and must be
/// released by it.
#[must_use = "dropping the guard unlocks the mutex immediately"]
pub struct ReentrantMutexGuard<'a, T: ?Sized> {
    lock: &'a ReentrantMutex<T>,
    // `*const ()` makes the guard neither `Send` nor `Sync`; `Sync` is added back below.
    _marker: PhantomData<(&'a T, *const ())>,
}

// SAFETY: sharing the guard shares only `&T`, which other threads may hold if `T: Sync`.
unsafe impl<T: ?Sized + Sync> Sync for ReentrantMutexGuard<'_, T> {}

impl<'a, T: ?Sized> ReentrantMutexGuard<'a, T> {
    fn new(lock: &'a ReentrantMutex<T>) -> Self {
        Self {
            lock,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for ReentrantMutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: this thread holds the lock, and every guard hands out only `&T`, so no `&mut T`
        // exists.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for ReentrantMutexGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        let lock = self.lock;
        // The guard is not `Send`, so this is the owning thread and may touch `count`.
        let count = lock.count.get() - 1;
        lock.count.set(count);
        if count == 0 {
            lock.owner.store(NO_OWNER, Relaxed);
            // SAFETY: this thread acquired `raw` when the count went from 0 to 1, and this is the
            // matching release.
            unsafe { lock.raw.unlock() };
        }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for ReentrantMutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for ReentrantMutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
