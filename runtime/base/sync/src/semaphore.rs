use core::fmt;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use core::sync::atomic::{AtomicU32, fence};
use core::time::Duration;

use marlin_pal::futex;

use crate::timeout::{self, Timeout};

/// A counting semaphore: a pool of permits that [`acquire`](Self::acquire) takes one of (blocking
/// while none is left) and [`release`](Self::release) returns.
///
/// Two 32-bit words: the permit count, which sleepers wait on, and a count of sleepers, so
/// `release` makes no syscall when nobody waits. Not fair: a thread arriving while a woken waiter
/// is on its way may take the permit first.
///
/// Permits are not tied to threads. Any thread may release, and may release permits it never
/// acquired (raising the count above its start). For scoped use, the `_permit` methods return a
/// [`SemaphorePermit`] that releases on drop.
pub struct Semaphore {
    permits: AtomicU32,
    waiters: AtomicU32,
}

impl Semaphore {
    #[inline]
    pub const fn new(permits: u32) -> Self {
        Self {
            permits: AtomicU32::new(permits),
            waiters: AtomicU32::new(0),
        }
    }

    /// Permits currently available. Only a hint: it may change immediately.
    #[inline]
    pub fn available_permits(&self) -> u32 {
        self.permits.load(Relaxed)
    }

    /// Takes a permit, blocking until one is available.
    #[inline]
    pub fn acquire(&self) {
        if !self.try_acquire() {
            self.acquire_slow(None);
        }
    }

    /// Takes a permit if one is available now.
    #[inline]
    pub fn try_acquire(&self) -> bool {
        self.permits
            .try_update(Acquire, Relaxed, |p| p.checked_sub(1))
            .is_ok()
    }

    /// Like [`acquire`](Self::acquire), giving up after `timeout`; returns `true` if a permit was
    /// taken.
    #[inline]
    pub fn try_acquire_for(&self, timeout: Duration) -> bool {
        self.try_acquire() || self.acquire_slow(Some(&Timeout::start(timeout)))
    }

    /// Returns `n` permits, waking waiters to take them.
    ///
    /// # Panics
    ///
    /// If the count would exceed `u32::MAX`.
    pub fn release(&self, n: u32) {
        if n == 0 {
            return;
        }
        let added = self
            .permits
            .try_update(Release, Relaxed, |p| p.checked_add(n));
        assert!(added.is_ok(), "semaphore permit count overflow");
        // Pairs with the fence in `acquire_slow`: either the waiter sees the new permits, or we
        // see the waiter.
        fence(SeqCst);
        if self.waiters.load(Relaxed) != 0 {
            if n == 1 {
                futex::wake_one(&self.permits);
            } else {
                futex::wake_all(&self.permits);
            }
        }
    }

    /// Takes a permit, blocking until one is available; the permit returns when the guard drops.
    #[inline]
    pub fn acquire_permit(&self) -> SemaphorePermit<'_> {
        self.acquire();
        SemaphorePermit { semaphore: self }
    }

    #[inline]
    pub fn try_acquire_permit(&self) -> Option<SemaphorePermit<'_>> {
        self.try_acquire()
            .then(|| SemaphorePermit { semaphore: self })
    }

    #[inline]
    pub fn try_acquire_permit_for(&self, timeout: Duration) -> Option<SemaphorePermit<'_>> {
        self.try_acquire_for(timeout)
            .then(|| SemaphorePermit { semaphore: self })
    }

    /// Returns `false` on timeout.
    #[cold]
    fn acquire_slow(&self, timeout: Option<&Timeout>) -> bool {
        loop {
            // Retry before checking the deadline, so a waiter woken for a permit never leaves
            // it behind: a wake consumed by a waiter that then times out found no permit to take.
            if self.try_acquire() {
                return true;
            }
            let Some(remaining) = timeout::wait_time(timeout) else {
                return false;
            };
            self.waiters.fetch_add(1, Relaxed);
            fence(SeqCst);
            futex::wait(&self.permits, 0, remaining);
            self.waiters.fetch_sub(1, Relaxed);
        }
    }
}

impl Default for Semaphore {
    /// A semaphore with no permits.
    fn default() -> Self {
        Self::new(0)
    }
}

impl fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Semaphore")
            .field("permits", &self.available_permits())
            .finish_non_exhaustive()
    }
}

/// One permit taken from a [`Semaphore`]; returned on drop.
#[must_use = "dropping the permit returns it immediately"]
pub struct SemaphorePermit<'a> {
    semaphore: &'a Semaphore,
}

impl SemaphorePermit<'_> {
    /// Keeps the permit taken: it is not returned on drop. Pair with a later
    /// [`Semaphore::release`] elsewhere.
    #[inline]
    pub fn forget(self) {
        core::mem::forget(self);
    }
}

impl Drop for SemaphorePermit<'_> {
    #[inline]
    fn drop(&mut self) {
        self.semaphore.release(1);
    }
}

impl fmt::Debug for SemaphorePermit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SemaphorePermit").finish_non_exhaustive()
    }
}
