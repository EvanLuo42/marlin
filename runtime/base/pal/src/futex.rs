//! Wait-on-address: the primitive every blocking lock in `marlin-sync` is built on.
//!
//! Windows `WaitOnAddress`, Linux `futex`, macOS `__ulock_wait`.

use core::sync::atomic::AtomicU32;
use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::Futex;

/// Blocks while `*addr == expected` until woken by [`wake_one`]/[`wake_all`], timed out, or woken
/// spuriously, so callers must re-check their condition in a loop. Returns `false` only on timeout.
#[inline]
pub fn wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool {
    Backend::wait(addr, expected, timeout)
}

/// Wakes at least one thread waiting on `addr`, if any.
#[inline]
pub fn wake_one(addr: &AtomicU32) {
    Backend::wake_one(addr)
}

/// Wakes every thread waiting on `addr`.
#[inline]
pub fn wake_all(addr: &AtomicU32) {
    Backend::wake_all(addr)
}
