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

// `state` layout:
//   bits 0..30  reader count, or MASK when write-locked
//   bit 30      readers are (or are about to be) asleep
//   bit 31      writers are (or are about to be) asleep
const READ_LOCKED: u32 = 1;
const MASK: u32 = (1 << 30) - 1;
const WRITE_LOCKED: u32 = MASK;
const MAX_READERS: u32 = MASK - 1;
const READERS_WAITING: u32 = 1 << 30;
const WRITERS_WAITING: u32 = 1 << 31;

#[inline]
fn is_unlocked(state: u32) -> bool {
    state & MASK == 0
}

#[inline]
fn is_write_locked(state: u32) -> bool {
    state & MASK == WRITE_LOCKED
}

#[inline]
fn has_readers_waiting(state: u32) -> bool {
    state & READERS_WAITING != 0
}

#[inline]
fn has_writers_waiting(state: u32) -> bool {
    state & WRITERS_WAITING != 0
}

/// New readers back off whenever anyone is waiting: waiting writers have priority (so a stream of
/// readers cannot starve them), and waiting readers mean an unlocker is busy deciding whom to wake.
#[inline]
fn is_read_lockable(state: u32) -> bool {
    state & MASK < MAX_READERS && !has_readers_waiting(state) && !has_writers_waiting(state)
}

/// A writer-preferring reader-writer lock without data: two 32-bit words.
///
/// Once a writer is waiting, new readers block until it has had its turn, so writers never starve.
/// The flip side: re-acquiring a read lock on a thread that already holds one can deadlock if a
/// writer arrives in between.
pub struct RawRwLock {
    state: AtomicU32,
    /// Bumped on every writer wakeup, so a writer about to sleep can tell it missed one.
    writer_notify: AtomicU32,
}

impl RawRwLock {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(0),
            writer_notify: AtomicU32::new(0),
        }
    }

    #[inline]
    pub fn try_read(&self) -> bool {
        self.state
            .try_update(Acquire, Relaxed, |s| {
                is_read_lockable(s).then(|| s + READ_LOCKED)
            })
            .is_ok()
    }

    #[inline]
    pub fn read(&self) {
        let state = self.state.load(Relaxed);
        if !is_read_lockable(state)
            || self
                .state
                .compare_exchange_weak(state, state + READ_LOCKED, Acquire, Relaxed)
                .is_err()
        {
            self.read_contended(None);
        }
    }

    /// Like [`read`](Self::read), giving up after `timeout`; returns `true` if a read lock was
    /// acquired.
    #[inline]
    pub fn try_read_for(&self, timeout: Duration) -> bool {
        self.try_read() || self.read_contended(Some(&Timeout::start(timeout)))
    }

    /// Releases one read lock.
    ///
    /// # Safety
    ///
    /// A read lock must be held, and the caller must be entitled to release it.
    #[inline]
    pub unsafe fn read_unlock(&self) {
        let state = self.state.fetch_sub(READ_LOCKED, Release) - READ_LOCKED;
        // Readers only sleep behind a writer (holding or waiting), so they cannot be waiting alone.
        debug_assert!(!has_readers_waiting(state) || has_writers_waiting(state));
        if is_unlocked(state) && has_writers_waiting(state) {
            self.wake_writer_or_readers(state);
        }
    }

    #[inline]
    pub fn try_write(&self) -> bool {
        self.state
            .try_update(Acquire, Relaxed, |s| {
                is_unlocked(s).then(|| s + WRITE_LOCKED)
            })
            .is_ok()
    }

    #[inline]
    pub fn write(&self) {
        if self
            .state
            .compare_exchange_weak(0, WRITE_LOCKED, Acquire, Relaxed)
            .is_err()
        {
            self.write_contended(None);
        }
    }

    /// Like [`write`](Self::write), giving up after `timeout`; returns `true` if the write lock
    /// was acquired.
    #[inline]
    pub fn try_write_for(&self, timeout: Duration) -> bool {
        self.try_write() || self.write_contended(Some(&Timeout::start(timeout)))
    }

    /// Releases the write lock.
    ///
    /// # Safety
    ///
    /// The write lock must be held, and the caller must be entitled to release it.
    #[inline]
    pub unsafe fn write_unlock(&self) {
        let state = self.state.fetch_sub(WRITE_LOCKED, Release) - WRITE_LOCKED;
        debug_assert!(is_unlocked(state));
        if has_writers_waiting(state) || has_readers_waiting(state) {
            self.wake_writer_or_readers(state);
        }
    }

    /// Whether any reader or writer holds the lock. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        !is_unlocked(self.state.load(Relaxed))
    }

    /// Returns `false` on timeout.
    #[cold]
    fn read_contended(&self, timeout: Option<&Timeout>) -> bool {
        let mut state = self.spin_read();
        loop {
            if is_read_lockable(state) {
                match self
                    .state
                    .compare_exchange_weak(state, state + READ_LOCKED, Acquire, Relaxed)
                {
                    Ok(_) => return true,
                    Err(s) => {
                        state = s;
                        continue;
                    }
                }
            }

            assert!(
                state & MASK != MAX_READERS,
                "too many concurrent read locks on RwLock"
            );

            // Announce ourselves before sleeping so the unlocker knows to wake readers.
            if !has_readers_waiting(state)
                && let Err(s) =
                    self.state
                        .compare_exchange(state, state | READERS_WAITING, Relaxed, Relaxed)
            {
                state = s;
                continue;
            }

            // A reader giving up leaves READERS_WAITING behind. That is harmless: readers only
            // sleep behind a writer, whose unlock (or the last reader's) clears the bit.
            let Some(remaining) = timeout::wait_time(timeout) else {
                return false;
            };
            futex::wait(&self.state, state | READERS_WAITING, remaining);
            state = self.spin_read();
        }
    }

    /// Returns `false` on timeout.
    #[cold]
    fn write_contended(&self, timeout: Option<&Timeout>) -> bool {
        let mut state = self.spin_write();
        // Once we have slept, other writers may be asleep too; keep their bit when we lock.
        let mut other_writers_waiting = 0;
        loop {
            if is_unlocked(state) {
                match self.state.compare_exchange_weak(
                    state,
                    state | WRITE_LOCKED | other_writers_waiting,
                    Acquire,
                    Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(s) => {
                        state = s;
                        continue;
                    }
                }
            }

            if !has_writers_waiting(state)
                && let Err(s) =
                    self.state
                        .compare_exchange(state, state | WRITERS_WAITING, Relaxed, Relaxed)
            {
                state = s;
                continue;
            }
            other_writers_waiting = WRITERS_WAITING;

            // Read the notify counter before re-checking `state`: a wake that lands between the
            // two then changes the counter and the wait below returns at once.
            let seq = self.writer_notify.load(Acquire);
            state = self.state.load(Relaxed);
            if is_unlocked(state) || !has_writers_waiting(state) {
                continue;
            }
            // Give up only here, having just seen the lock held with WRITERS_WAITING set: if a
            // writer wakeup reached us, the holder's unlock will issue another for the rest. The
            // bit itself may stay set for nobody; the next unlock clears it.
            let Some(remaining) = timeout::wait_time(timeout) else {
                return false;
            };
            futex::wait(&self.writer_notify, seq, remaining);
            state = self.spin_write();
        }
    }

    /// Called by the last unlocker with `state` unlocked and someone waiting. If anyone locks it
    /// meanwhile, the CASes fail and waking becomes that thread's job on its unlock.
    #[cold]
    fn wake_writer_or_readers(&self, mut state: u32) {
        debug_assert!(is_unlocked(state));

        if state == WRITERS_WAITING {
            match self.state.compare_exchange(state, 0, Relaxed, Relaxed) {
                Ok(_) => {
                    self.wake_writer();
                    return;
                }
                Err(s) => state = s,
            }
        }

        // Both waiting: writers first. The PAL cannot tell us whether a writer was actually
        // asleep to receive the wake (it may still be on its way to sleep, and will then see the
        // bumped counter), so wake the readers too rather than risk leaving them asleep with
        // nobody left to wake them. The writer that wins re-sets its bit and holds new readers
        // back again.
        if state == READERS_WAITING + WRITERS_WAITING {
            if self
                .state
                .compare_exchange(state, READERS_WAITING, Relaxed, Relaxed)
                .is_err()
            {
                return;
            }
            self.wake_writer();
            state = READERS_WAITING;
        }

        if state == READERS_WAITING
            && self
                .state
                .compare_exchange(state, 0, Relaxed, Relaxed)
                .is_ok()
        {
            futex::wake_all(&self.state);
        }
    }

    fn wake_writer(&self) {
        self.writer_notify.fetch_add(1, Release);
        futex::wake_one(&self.writer_notify);
    }

    fn spin_until(&self, done: impl Fn(u32) -> bool) -> u32 {
        let mut spins = SPIN_LIMIT;
        loop {
            let state = self.state.load(Relaxed);
            if done(state) || spins == 0 {
                return state;
            }
            core::hint::spin_loop();
            spins -= 1;
        }
    }

    fn spin_write(&self) -> u32 {
        // Stop once unlocked, or once others are asleep (no point queueing behind them by spinning).
        self.spin_until(|s| is_unlocked(s) || has_writers_waiting(s))
    }

    fn spin_read(&self) -> u32 {
        self.spin_until(|s| !is_write_locked(s) || has_readers_waiting(s) || has_writers_waiting(s))
    }
}

impl Default for RawRwLock {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for RawRwLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawRwLock")
            .field("locked", &self.is_locked())
            .finish()
    }
}

/// A writer-preferring reader-writer lock protecting a `T`. See [`RawRwLock`].
pub struct RwLock<T: ?Sized> {
    raw: RawRwLock,
    data: UnsafeCell<T>,
}

// SAFETY: a write guard gives one thread `&mut T` (needs `Send`); read guards give many threads
// `&T` at once (needs `Sync`).
unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawRwLock::new(),
            data: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Blocks until a shared lock is acquired. Blocks behind waiting writers, even if readers
    /// currently hold the lock.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.raw.read();
        RwLockReadGuard::new(self)
    }

    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.raw.try_read().then(|| RwLockReadGuard::new(self))
    }

    /// Like [`read`](Self::read), giving up after `timeout`.
    #[inline]
    pub fn try_read_for(&self, timeout: Duration) -> Option<RwLockReadGuard<'_, T>> {
        self.raw
            .try_read_for(timeout)
            .then(|| RwLockReadGuard::new(self))
    }

    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.raw.write();
        RwLockWriteGuard::new(self)
    }

    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.raw.try_write().then(|| RwLockWriteGuard::new(self))
    }

    /// Like [`write`](Self::write), giving up after `timeout`.
    #[inline]
    pub fn try_write_for(&self, timeout: Duration) -> Option<RwLockWriteGuard<'_, T>> {
        self.raw
            .try_write_for(timeout)
            .then(|| RwLockWriteGuard::new(self))
    }

    /// Whether the lock is held. Only a hint: it may change immediately.
    #[inline]
    pub fn is_locked(&self) -> bool {
        self.raw.is_locked()
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for RwLock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
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

/// Holds a shared lock on an [`RwLock`]; unlocks on drop.
#[must_use = "dropping the guard unlocks the lock immediately"]
pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    // Auto traits of `&T`.
    _marker: PhantomData<&'a T>,
}

impl<'a, T: ?Sized> RwLockReadGuard<'a, T> {
    fn new(lock: &'a RwLock<T>) -> Self {
        Self {
            lock,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: a shared lock is held, so no `&mut T` exists.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: the guard was created by acquiring a read lock and is dropped exactly once.
        unsafe { self.lock.raw.read_unlock() }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

/// Holds the exclusive lock on an [`RwLock`]; unlocks on drop.
#[must_use = "dropping the guard unlocks the lock immediately"]
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    // Auto traits of `&mut T`.
    _marker: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> RwLockWriteGuard<'a, T> {
    fn new(lock: &'a RwLock<T>) -> Self {
        Self {
            lock,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the write lock is held, so no other reference to the data is live.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: the guard was created by acquiring the write lock and is dropped exactly once.
        unsafe { self.lock.raw.write_unlock() }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
