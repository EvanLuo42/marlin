//! Job counters: what jobs and outside threads wait on, and what dependent jobs start after.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_std::sync::{Condvar, Mutex};

use crate::scheduler::{self, Fiber, Job, Priority, Shared};

/// Counts unfinished jobs. Waiting for zero parks the fiber on a worker, or blocks an outside
/// thread; continuations registered on it are released at zero. The count may rise again after
/// reaching zero (a reused counter), and each time it reaches zero everyone then waiting goes.
///
/// Shared through an `Arc` by its waiters and every job it counts, so the last decrement may
/// still touch it after the waiter has moved on.
pub(crate) struct Counter {
    value: AtomicU32,
    waiters: Mutex<Waiters>,
    /// For threads that are not workers.
    zero: Condvar,
}

pub(crate) struct Waiters {
    /// Fibers parked until the count reaches zero, linked through `Fiber::next`.
    fibers: *mut Fiber,
    continuations: Vec<Arc<Continuation>>,
}

// SAFETY: the list is only touched under the mutex; the fibers it links are parked.
unsafe impl Send for Waiters {}

impl Counter {
    pub(crate) const fn new() -> Self {
        Self {
            value: AtomicU32::new(0),
            waiters: Mutex::new(Waiters {
                fibers: null_mut(),
                continuations: Vec::new(),
            }),
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
        let (mut fiber, continuations) = {
            let mut waiters = self.waiters.lock();
            self.zero.notify_all();
            (
                core::mem::replace(&mut waiters.fibers, null_mut()),
                core::mem::take(&mut waiters.continuations),
            )
        };
        while !fiber.is_null() {
            // SAFETY: parked fibers are only reachable through this list until made ready.
            unsafe {
                let next = (*fiber).next;
                scheduler::make_ready(fiber);
                fiber = next;
            }
        }
        for continuation in continuations {
            continuation.release();
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
            unsafe { (*fiber).next = waiters.fibers };
            waiters.fibers = fiber;
        }
    }

    /// Releases `continuation` once the count is zero (at once if it already is).
    pub(crate) fn add_continuation(&self, continuation: &Arc<Continuation>) {
        let mut waiters = self.waiters.lock();
        if self.is_zero() {
            drop(waiters);
            continuation.release();
        } else {
            waiters.continuations.push(continuation.clone());
        }
    }
}

/// A job queued once `remaining` releases have happened: one per unfinished dependency, plus
/// one held by its creator while it registers them.
pub(crate) struct Continuation {
    remaining: AtomicU32,
    /// Taken by the release that brings `remaining` to zero.
    job: UnsafeCell<Option<Job>>,
    priority: Priority,
    shared: *const Shared,
}

// SAFETY: `job` is written at creation and taken exactly once, by the thread whose release
// brought `remaining` to zero (the AcqRel decrements order it after every other access).
// `shared` outlives the continuation: the job system waits for its detached work, which counts
// this job, before it is dropped.
unsafe impl Send for Continuation {}
// SAFETY: as above.
unsafe impl Sync for Continuation {}

impl Continuation {
    pub(crate) fn new(
        dependencies: u32,
        job: Job,
        priority: Priority,
        shared: &Shared,
    ) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicU32::new(dependencies + 1),
            job: UnsafeCell::new(Some(job)),
            priority,
            shared,
        })
    }

    pub(crate) fn release(&self) {
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        // SAFETY: the last release; nothing else touches `job` (see the `Sync` impl), and the
        // job system outlives its detached work.
        unsafe {
            let job = (*self.job.get())
                .take()
                .expect("continuation released twice");
            (*self.shared).push_job(job, self.priority);
        }
    }
}
