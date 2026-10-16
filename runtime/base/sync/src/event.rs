use core::fmt;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::time::Duration;

use marlin_pal::futex;

use crate::timeout::Timeout;

/// What a successful wait does to an [`Event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    /// Stays set, releasing every waiter, until [`Event::reset`].
    Manual,
    /// A successful wait consumes the signal: each `set` releases exactly one waiter.
    Auto,
}

const SET: u32 = 1;

// Manual: bit 1 says someone may be asleep (so `set` must wake), bits 2.. count `set`s. A waiter
// that sees the count move returns even if the event was reset again before it woke up, so a
// quick set-reset pulse still releases everyone who was waiting at the time.
const MANUAL_WAITERS: u32 = 1 << 1;
const MANUAL_GEN: u32 = 1 << 2;
const MANUAL_GEN_MASK: u32 = !(SET | MANUAL_WAITERS);

// Auto: bits 1.. count the waiters, so `set` wakes only when someone is there to consume it.
const AUTO_WAITER: u32 = 1 << 1;

/// A signal threads can wait on, like a Win32 event object, in one 32-bit word.
///
/// Setting an event that is already set does nothing: signals do not accumulate.
pub struct Event {
    state: AtomicU32,
    mode: ResetMode,
}

impl Event {
    #[inline]
    pub const fn new(mode: ResetMode, set: bool) -> Self {
        Self {
            state: AtomicU32::new(if set { SET } else { 0 }),
            mode,
        }
    }

    /// An unset [`ResetMode::Manual`] event.
    #[inline]
    pub const fn manual() -> Self {
        Self::new(ResetMode::Manual, false)
    }

    /// An unset [`ResetMode::Auto`] event.
    #[inline]
    pub const fn auto() -> Self {
        Self::new(ResetMode::Auto, false)
    }

    #[inline]
    pub fn mode(&self) -> ResetMode {
        self.mode
    }

    /// Whether the event is set. Only a hint: it may change immediately.
    #[inline]
    pub fn is_set(&self) -> bool {
        self.state.load(Relaxed) & SET != 0
    }

    /// Signals the event. Manual: releases all current and future waiters until reset. Auto:
    /// releases one waiter, or the next thread to wait if none is waiting now.
    pub fn set(&self) {
        match self.mode {
            ResetMode::Manual => {
                let old = self.state.try_update(Release, Relaxed, |s| {
                    (s & SET == 0).then(|| (s & MANUAL_GEN_MASK).wrapping_add(MANUAL_GEN) | SET)
                });
                if let Ok(old) = old
                    && old & MANUAL_WAITERS != 0
                {
                    futex::wake_all(&self.state);
                }
            }
            ResetMode::Auto => {
                let old = self.state.fetch_or(SET, Release);
                if old & SET == 0 && old >= AUTO_WAITER {
                    futex::wake_one(&self.state);
                }
            }
        }
    }

    /// Clears the signal. Rarely needed for auto-reset events, whose waits clear it.
    #[inline]
    pub fn reset(&self) {
        self.state.fetch_and(!SET, Relaxed);
    }

    /// Blocks until the event is signaled.
    #[inline]
    pub fn wait(&self) {
        self.wait_inner(None);
    }

    /// Blocks until the event is signaled or `timeout` elapses; returns `false` on timeout.
    /// A zero timeout just polls (and, for auto-reset, consumes the signal if set).
    #[inline]
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        self.wait_inner(Some(Timeout::start(timeout)))
    }

    fn wait_inner(&self, timeout: Option<Timeout>) -> bool {
        match self.mode {
            ResetMode::Manual => self.wait_manual(timeout),
            ResetMode::Auto => self.wait_auto(timeout),
        }
    }

    fn wait_manual(&self, timeout: Option<Timeout>) -> bool {
        let mut state = self.state.load(Acquire);
        if state & SET != 0 {
            return true;
        }
        let generation = state & MANUAL_GEN_MASK;
        loop {
            if state & SET != 0 || state & MANUAL_GEN_MASK != generation {
                return true;
            }
            if state & MANUAL_WAITERS == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | MANUAL_WAITERS,
                    Relaxed,
                    Acquire,
                ) {
                    Ok(_) => state |= MANUAL_WAITERS,
                    Err(s) => {
                        state = s;
                        continue;
                    }
                }
            }
            let remaining = match &timeout {
                None => None,
                Some(t) => match t.remaining() {
                    Some(left) => Some(left),
                    None => return false,
                },
            };
            futex::wait(&self.state, state, remaining);
            state = self.state.load(Acquire);
        }
    }

    fn wait_auto(&self, timeout: Option<Timeout>) -> bool {
        if self.try_consume() {
            return true;
        }
        // Register so `set` knows to wake; every exit below unregisters.
        let mut state = self.state.fetch_add(AUTO_WAITER, Relaxed) + AUTO_WAITER;
        loop {
            if state & SET != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    (state & !SET) - AUTO_WAITER,
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
            let remaining = match &timeout {
                None => None,
                Some(t) => match t.remaining() {
                    Some(left) => Some(left),
                    None => {
                        self.state.fetch_sub(AUTO_WAITER, Relaxed);
                        return false;
                    }
                },
            };
            futex::wait(&self.state, state, remaining);
            state = self.state.load(Relaxed);
        }
    }

    fn try_consume(&self) -> bool {
        self.state
            .try_update(Acquire, Relaxed, |s| (s & SET != 0).then_some(s & !SET))
            .is_ok()
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Event")
            .field("mode", &self.mode)
            .field("set", &self.is_set())
            .finish()
    }
}
