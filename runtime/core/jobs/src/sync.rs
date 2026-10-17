//! Fiber-aware locks and signals: [`Mutex`], [`RwLock`], [`Condvar`] and [`Event`].
//!
//! Contended inside a job, they park the job's fiber and the worker runs other jobs meanwhile;
//! on the registered main thread they run jobs too (see [`JobSystem::main_thread`]); on any
//! other thread they block it. Their guards may be held across job waits and dropped on another
//! worker thread than the one that locked: fibers migrate, and these locks have no owning thread.
//!
//! **Use these, not `marlin_std::sync`, for anything held across a job wait** (a scope, a
//! [`JobHandle::wait`](crate::JobHandle::wait), or another of these locks). A `marlin_std::sync`
//! lock blocks its whole worker thread when contended, and a job that waits while holding one
//! parks with the lock held: another job on the same worker that wants it then blocks the
//! thread that would resume the holder. `marlin_std::sync` locks remain the right choice for
//! short critical sections that never wait; uncontended, both kinds cost one atomic operation.
//!
//! Waiters are served in FIFO order and a released lock is handed directly to the next waiter, so
//! nobody starves. At the fiber cap (see the crate docs) a contended lock no longer parks the
//! fiber: the waiter sleeps with the idle workers, resuming any fiber that becomes ready (which
//! may be the lock's holder) but running no new jobs.
//!
//! [`JobSystem::main_thread`]: crate::JobSystem::main_thread

use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::wait::WaitQueue;

/// Spins this many times on a held lock before waiting: enough to ride out a short critical
/// section on another core.
const SPIN_LIMIT: u32 = 100;

// Lock state: a writer bit, a bit saying the wait queue is not empty, and the reader count.
const WRITER: u32 = 1 << 31;
const QUEUED: u32 = 1 << 30;
const READERS: u32 = QUEUED - 1;

// Wait node kinds.
const EXCLUSIVE: u32 = 0;
const SHARED: u32 = 1;

/// The lock under [`Mutex`] and [`RwLock`]: one word, and a FIFO of waiters.
///
/// Under the queue's lock, `QUEUED` is set exactly when the queue is not empty. Fast paths never
/// touch a lock with `QUEUED` set (new readers queue behind a waiting writer), and unlocking it
/// hands the lock to the first waiter (or to every reader at the front) without unlocking.
struct RawLock {
    state: AtomicU32,
    queue: WaitQueue<()>,
}

impl RawLock {
    const fn new() -> Self {
        Self {
            state: AtomicU32::new(0),
            queue: WaitQueue::new(()),
        }
    }

    fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn lock(&self) {
        if !self.try_lock() && !self.spin(|| self.try_lock()) {
            self.lock_slow(EXCLUSIVE);
        }
    }

    fn try_lock_shared(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & (WRITER | QUEUED) != 0 || state & READERS == READERS {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(s) => state = s,
            }
        }
    }

    fn lock_shared(&self) {
        if !self.try_lock_shared() && !self.spin(|| self.try_lock_shared()) {
            self.lock_slow(SHARED);
        }
    }

    /// Spins while the lock is held and nobody is queued (then waiting is better).
    fn spin(&self, mut try_lock: impl FnMut() -> bool) -> bool {
        for _ in 0..SPIN_LIMIT {
            if self.state.load(Ordering::Relaxed) & QUEUED != 0 {
                return false;
            }
            core::hint::spin_loop();
            if try_lock() {
                return true;
            }
        }
        false
    }

    #[cold]
    fn lock_slow(&self, kind: u32) {
        // Under the queue lock: take the lock if it is free, else mark the queue non-empty and
        // queue up. A granted waiter owns the lock: the unlocker handed it over.
        self.queue.wait(kind, false, |()| {
            loop {
                let state = self.state.load(Ordering::Relaxed);
                let (free, taken) = match kind {
                    EXCLUSIVE => (state == 0, WRITER),
                    _ => (
                        state & (WRITER | QUEUED) == 0 && state & READERS != READERS,
                        state + 1,
                    ),
                };
                let new = if free { taken } else { state | QUEUED };
                if self
                    .state
                    .compare_exchange_weak(state, new, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return !free;
                }
            }
        });
    }

    /// # Safety
    /// Held exclusively by the caller (or whoever handed it to the caller).
    unsafe fn unlock(&self) {
        if self
            .state
            .compare_exchange(WRITER, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.hand_over(WRITER);
        }
    }

    /// # Safety
    /// Held shared by the caller.
    unsafe fn unlock_shared(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            // The last reader with waiters queued hands the lock over.
            if state & QUEUED != 0 && state & READERS == 1 {
                self.hand_over(1);
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state - 1,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(s) => state = s,
            }
        }
    }

    /// Passes the lock, held as `held` with waiters queued, to the first waiter, or to every
    /// reader at the front of the queue.
    #[cold]
    fn hand_over(&self, held: u32) {
        let mut queue = self.queue.lock();
        // Nobody else can change the state now: it is held, and `QUEUED` stops the fast paths.
        debug_assert_eq!(
            self.state.load(Ordering::Relaxed) & !QUEUED,
            held,
            "lock state"
        );
        let state = match queue.front() {
            Some(EXCLUSIVE) => {
                queue.grant_front();
                WRITER
            }
            Some(_) => {
                let mut readers = 0;
                while queue.front() == Some(SHARED) {
                    queue.grant_front();
                    readers += 1;
                }
                readers
            }
            None => unreachable!("QUEUED with an empty queue"),
        };
        let state = if queue.is_empty() {
            state
        } else {
            state | QUEUED
        };
        // Release: the new holders see what the old one wrote. (The queue lock orders it too.)
        self.state.store(state, Ordering::Release);
    }

    fn is_locked(&self) -> bool {
        self.state.load(Ordering::Relaxed) & (WRITER | READERS) != 0
    }
}

/// A fiber-aware mutual-exclusion lock protecting a `T`. See the [module docs](self).
pub struct Mutex<T: ?Sized> {
    raw: RawLock,
    data: UnsafeCell<T>,
}

// SAFETY: the lock hands out access to `T` to one context at a time, so it is enough that `T`
// can be sent between threads.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawLock::new(),
            data: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Locks, parking the fiber (in a job) or blocking the thread while it is held. Locking a
    /// mutex the caller already holds deadlocks.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard::new(self)
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw.try_lock().then(|| MutexGuard::new(self))
    }

    /// Whether the lock is held. Only a hint: it may change immediately.
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    /// Access without locking: `&mut self` proves no guard exists.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
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

/// Holds a [`Mutex`] locked; unlocks on drop, on whichever thread that happens.
#[must_use = "dropping the guard unlocks the mutex immediately"]
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    // The auto traits of `&mut T`: Send if `T: Send`, Sync only if `T: Sync`.
    _marker: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock, so no other reference to the data is live.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
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

/// A fiber-aware reader-writer lock protecting a `T`: many readers or one writer. Fair: once a
/// writer waits, new readers queue behind it. See the [module docs](self).
pub struct RwLock<T: ?Sized> {
    raw: RawLock,
    data: UnsafeCell<T>,
}

// SAFETY: writers get exclusive access (needs `Send`), readers shared access from many contexts
// at once (needs `Sync`).
unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawLock::new(),
            data: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.raw.lock_shared();
        RwLockReadGuard {
            lock: self,
            _marker: PhantomData,
        }
    }

    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.raw.try_lock_shared().then(|| RwLockReadGuard {
            lock: self,
            _marker: PhantomData,
        })
    }

    /// Locking for writing while the caller holds a read or write guard deadlocks.
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.raw.lock();
        RwLockWriteGuard {
            lock: self,
            _marker: PhantomData,
        }
    }

    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.raw.try_lock().then(|| RwLockWriteGuard {
            lock: self,
            _marker: PhantomData,
        })
    }

    /// Whether the lock is held. Only a hint: it may change immediately.
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RwLock");
        match self.try_read() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &format_args!("<locked>")),
        };
        d.finish_non_exhaustive()
    }
}

#[must_use = "dropping the guard unlocks the lock immediately"]
pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    // The auto traits of `&T`.
    _marker: PhantomData<&'a T>,
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard holds a read lock, so no writer exists.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: the guard holds a read lock and is dropped exactly once.
        unsafe { self.lock.raw.unlock_shared() }
    }
}

#[must_use = "dropping the guard unlocks the lock immediately"]
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    // The auto traits of `&mut T`.
    _marker: PhantomData<&'a mut T>,
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard holds the write lock: no other reference to the data is live.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: the guard holds the write lock and is dropped exactly once.
        unsafe { self.lock.raw.unlock() }
    }
}

/// A fiber-aware condition variable for use with [`Mutex`]. Waits never return spuriously, but
/// the condition may have changed again by the time the mutex is relocked: check it in a loop
/// (or use [`wait_while`](Self::wait_while)).
pub struct Condvar {
    queue: WaitQueue<()>,
}

impl Condvar {
    pub const fn new() -> Self {
        Self {
            queue: WaitQueue::new(()),
        }
    }

    /// Unlocks the guard's mutex, waits until notified, and relocks it.
    pub fn wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        let raw = &guard.mutex.raw;
        // Unlocked under our queue's lock, after which notifications find us queued.
        self.queue.wait(0, false, |()| {
            // SAFETY: the guard holds the lock; it is relocked below before the guard is used.
            unsafe { raw.unlock() };
            true
        });
        raw.lock();
    }

    /// Waits until `condition` returns false.
    pub fn wait_while<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
    ) {
        while condition(&mut **guard) {
            self.wait(guard);
        }
    }

    /// Wakes the longest waiting waiter, if any.
    pub fn notify_one(&self) {
        self.queue.lock().grant_front();
    }

    pub fn notify_all(&self) {
        self.queue.lock().grant_all();
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Condvar").finish_non_exhaustive()
    }
}

/// A fiber-aware manual-reset event: a gate that releases every waiter while it is set.
pub struct Event {
    set: AtomicBool,
    queue: WaitQueue<()>,
}

impl Event {
    pub const fn new(set: bool) -> Self {
        Self {
            set: AtomicBool::new(set),
            queue: WaitQueue::new(()),
        }
    }

    /// Sets the event, releasing every waiter, until [`reset`](Self::reset).
    pub fn set(&self) {
        let mut queue = self.queue.lock();
        self.set.store(true, Ordering::Release);
        queue.grant_all();
    }

    pub fn reset(&self) {
        let _queue = self.queue.lock();
        self.set.store(false, Ordering::Relaxed);
    }

    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::Acquire)
    }

    /// Waits until the event is set. A waiter released by [`set`](Self::set) returns even if
    /// the event was reset before it ran.
    pub fn wait(&self) {
        if self.is_set() {
            return;
        }
        self.queue
            .wait(0, false, |()| !self.set.load(Ordering::Acquire));
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new(false)
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Event")
            .field("set", &self.is_set())
            .finish_non_exhaustive()
    }
}
