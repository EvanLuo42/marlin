use core::fmt;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::Relaxed;
use core::time::Duration;

use marlin_pal::futex;

use crate::MutexGuard;
use crate::timeout::Timeout;

/// A condition variable for use with [`Mutex`](crate::Mutex).
///
/// One 32-bit sequence counter: notifying bumps it and wakes, waiting sleeps until it moves. The
/// waiter reads the counter while still holding the mutex, so a notify issued after the waiter's
/// condition check (which needs the mutex) always changes the counter before the waiter sleeps.
/// Waits may return spuriously; use the `_while` variants or re-check in a loop.
pub struct Condvar {
    seq: AtomicU32,
}

/// Whether a timed wait returned because its timeout elapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    #[inline]
    pub fn timed_out(self) -> bool {
        self.0
    }
}

impl Condvar {
    #[inline]
    pub const fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
        }
    }

    /// Wakes one waiting thread, if any.
    #[inline]
    pub fn notify_one(&self) {
        self.seq.fetch_add(1, Relaxed);
        futex::wake_one(&self.seq);
    }

    /// Wakes every waiting thread.
    #[inline]
    pub fn notify_all(&self) {
        self.seq.fetch_add(1, Relaxed);
        futex::wake_all(&self.seq);
    }

    /// Unlocks the guard's mutex, sleeps until notified (or spuriously woken), and relocks it.
    pub fn wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        self.wait_inner(guard, None);
    }

    /// Like [`wait`](Self::wait), giving up after `timeout`.
    pub fn wait_timeout<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        WaitTimeoutResult(!self.wait_inner(guard, Some(timeout)))
    }

    /// Waits until `condition` returns `false`, absorbing spurious wakeups.
    pub fn wait_while<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
    ) {
        while condition(&mut **guard) {
            self.wait(guard);
        }
    }

    /// Waits until `condition` returns `false` or `timeout` has elapsed in total. Times out only
    /// if the condition still holds at the end.
    pub fn wait_timeout_while<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
        mut condition: impl FnMut(&mut T) -> bool,
    ) -> WaitTimeoutResult {
        let timeout = Timeout::start(timeout);
        while condition(&mut **guard) {
            let Some(remaining) = timeout.remaining() else {
                return WaitTimeoutResult(true);
            };
            self.wait_inner(guard, Some(remaining));
        }
        WaitTimeoutResult(false)
    }

    /// Returns `false` on timeout.
    fn wait_inner<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Option<Duration>,
    ) -> bool {
        let seq = self.seq.load(Relaxed);
        let raw = guard.raw();
        // SAFETY: the guard holds the mutex. It is relocked below before the guard can be used
        // or dropped again; the `&mut` borrow keeps anyone else from touching the guard meanwhile.
        unsafe { raw.unlock() };
        let woken = futex::wait(&self.seq, seq, timeout);
        raw.lock();
        woken
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
