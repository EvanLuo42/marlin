use core::fmt;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::time::Duration;

use marlin_pal::futex;

use crate::timeout::Timeout;

const EMPTY: u32 = 0;
const NOTIFIED: u32 = 1;
const PARKED: u32 = 2;

/// A single wakeup token, the building block for a thread or job scheduler: one consumer
/// [`park`](Self::park)s, any thread [`unpark`](Self::unpark)s.
///
/// `unpark` before `park` is not lost: the token is stored and the next `park` returns at once.
/// Tokens do not accumulate. Parking from two threads at the same time is a bug and panics.
pub struct Parker {
    state: AtomicU32,
}

impl Parker {
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
        }
    }

    /// Blocks until the token is available, then consumes it. Never returns spuriously.
    pub fn park(&self) {
        if self.begin_park() {
            return;
        }
        loop {
            futex::wait(&self.state, PARKED, None);
            if self.try_consume() {
                return;
            }
        }
    }

    /// Like [`park`](Self::park), giving up after `timeout`. Returns `true` if the token was
    /// consumed.
    pub fn park_timeout(&self, timeout: Duration) -> bool {
        if self.begin_park() {
            return true;
        }
        let timeout = Timeout::start(timeout);
        while let Some(remaining) = timeout.remaining() {
            futex::wait(&self.state, PARKED, Some(remaining));
            if self.try_consume() {
                return true;
            }
        }
        // Leave PARKED. Only `unpark` can have changed it, to NOTIFIED, so take that token.
        self.state.swap(EMPTY, Acquire) == NOTIFIED
    }

    /// Makes the token available, waking the parked thread if there is one.
    #[inline]
    pub fn unpark(&self) {
        if self.state.swap(NOTIFIED, Release) == PARKED {
            futex::wake_one(&self.state);
        }
    }

    /// Consumes a waiting token (returns `true`) or moves to PARKED.
    fn begin_park(&self) -> bool {
        let mut state = self.state.load(Relaxed);
        loop {
            let (expected, new) = match state {
                NOTIFIED => (NOTIFIED, EMPTY),
                EMPTY => (EMPTY, PARKED),
                _ => panic!("Parker parked from two threads at once"),
            };
            match self
                .state
                .compare_exchange_weak(expected, new, Acquire, Relaxed)
            {
                Ok(_) => return state == NOTIFIED,
                Err(s) => state = s,
            }
        }
    }

    fn try_consume(&self) -> bool {
        self.state
            .compare_exchange(NOTIFIED, EMPTY, Acquire, Relaxed)
            .is_ok()
    }
}

impl Default for Parker {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Parker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parker").finish_non_exhaustive()
    }
}
