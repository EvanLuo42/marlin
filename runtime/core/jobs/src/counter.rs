//! Job counters: what jobs and outside threads wait on.

use core::ptr::null_mut;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_std::sync::{Condvar, Mutex};

use crate::scheduler::{self, Fiber};

/// Counts unfinished jobs. Waiting for zero parks the fiber on a worker, or blocks an outside
/// thread.
///
/// Shared through an `Arc` by its waiter and every job it counts, so the last decrement may
/// still touch it after the waiter has moved on.
pub(crate) struct Counter {
    value: AtomicU32,
    /// Fibers parked until `value` reaches zero, linked through `Fiber::next`.
    waiters: Mutex<WaiterList>,
    /// For threads that are not workers.
    zero: Condvar,
}

pub(crate) struct WaiterList(*mut Fiber);

// SAFETY: the list is only touched under the mutex; the fibers it links are parked.
unsafe impl Send for WaiterList {}

impl Counter {
    pub(crate) const fn new() -> Self {
        Self {
            value: AtomicU32::new(0),
            waiters: Mutex::new(WaiterList(null_mut())),
            zero: Condvar::new(),
        }
    }

    pub(crate) fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Releases everyone waiting once the count reaches zero.
    pub(crate) fn decrement(&self) {
        if self.value.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let mut fiber = {
            let mut waiters = self.waiters.lock();
            self.zero.notify_all();
            core::mem::replace(&mut waiters.0, null_mut())
        };
        while !fiber.is_null() {
            // SAFETY: parked fibers are only reachable through this list until made ready.
            unsafe {
                let next = (*fiber).next;
                scheduler::make_ready(fiber);
                fiber = next;
            }
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.value.load(Ordering::Acquire) == 0
    }

    /// Waits until the count is zero: parks the current fiber when called from a job, blocks the
    /// thread otherwise.
    pub(crate) fn wait(&self) {
        if self.is_zero() || scheduler::park_current_until_zero(self) {
            return;
        }
        let mut waiters = self.waiters.lock();
        while !self.is_zero() {
            self.zero.wait(&mut waiters);
        }
    }

    /// Parks `fiber` until the count is zero, or makes it ready at once if it already is.
    /// Called by the fiber that took over from `fiber`, after `fiber`'s context was saved.
    pub(crate) fn add_waiter(&self, fiber: *mut Fiber) {
        let mut waiters = self.waiters.lock();
        // Under the lock, so `decrement` either finds this waiter or we see zero.
        if self.is_zero() {
            drop(waiters);
            // SAFETY: the fiber is parked and ours to hand over.
            unsafe { scheduler::make_ready(fiber) };
        } else {
            // SAFETY: the fiber is parked and owned by this list from now on.
            unsafe { (*fiber).next = waiters.0 };
            waiters.0 = fiber;
        }
    }
}
