//! Job counters: what jobs and outside threads wait on, and what dependent jobs start after.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::scheduler::{Job, Priority, Shared};
use crate::wait::{self, Queue, Wait, WaitNode, WaitQueue};

/// Counts unfinished jobs. Waiting for zero parks the fiber on a worker, or blocks an outside
/// thread; continuations registered on it are released at zero. The count may rise again after
/// reaching zero (a reused counter), and each time it reaches zero everyone then waiting goes.
///
/// Shared through an `Arc` by its waiters and every job it counts, so the last decrement may
/// still touch it after the waiter has moved on.
pub(crate) struct Counter {
    value: AtomicU32,
    /// Set by `JobHandle::cancel`; see `handle.rs`.
    pub(crate) cancelled: AtomicBool,
    /// Waiters, and the continuations to release at zero.
    waiters: WaitQueue<Vec<Arc<Continuation>>>,
}

impl Counter {
    pub(crate) const fn new() -> Self {
        Self {
            value: AtomicU32::new(0),
            cancelled: AtomicBool::new(false),
            waiters: WaitQueue::new(Vec::new()),
        }
    }

    pub(crate) fn increment(&self) {
        self.add(1);
    }

    pub(crate) fn add(&self, n: u32) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Releases everyone waiting once the count reaches zero.
    pub(crate) fn decrement(&self) {
        if self.value.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let continuations = {
            let mut waiters = self.waiters.lock();
            waiters.grant_all();
            core::mem::take(waiters.state())
        };
        for continuation in continuations {
            continuation.release();
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.value.load(Ordering::Acquire) == 0
    }

    /// Waits until the count is zero: parks the current fiber when called from a job (or from
    /// the main thread, which runs jobs meanwhile), blocks the thread otherwise.
    pub(crate) fn wait(&self) {
        if !self.is_zero() {
            // Counters may help at the fiber cap: a counter's work is queued jobs.
            wait::block(self, 0, true);
        }
    }

    /// Releases `continuation` once the count is zero (at once if it already is).
    pub(crate) fn add_continuation(&self, continuation: &Arc<Continuation>) {
        let mut waiters = self.waiters.lock();
        // Under the lock, so `decrement` either finds this continuation or we see zero.
        if self.is_zero() {
            drop(waiters);
            continuation.release();
        } else {
            waiters.state().push(continuation.clone());
        }
    }
}

impl Wait for Counter {
    fn register(&self, node: *mut WaitNode) -> bool {
        let mut waiters = self.waiters.lock();
        // Under the lock, so `decrement` either grants this node or we see zero.
        if self.is_zero() {
            return false;
        }
        waiters.push(node);
        true
    }

    fn queue(&self) -> &dyn Queue {
        &self.waiters
    }
}

/// Where a job goes once it may run.
#[derive(Clone, Copy)]
pub(crate) enum Target {
    Queue(Priority),
    /// The main thread's queue.
    Main,
}

impl Target {
    pub(crate) fn push(self, shared: &Shared, job: Job) {
        match self {
            Target::Queue(priority) => shared.push_job(job, priority),
            Target::Main => shared.push_main(job),
        }
    }
}

/// A job queued once `remaining` releases have happened: one per unfinished dependency, plus
/// one held by its creator while it registers them.
pub(crate) struct Continuation {
    remaining: AtomicU32,
    /// Taken by the release that brings `remaining` to zero.
    job: UnsafeCell<Option<Job>>,
    target: Target,
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
    /// Holds only the creator's share: [`retain`](Self::retain) once per dependency.
    pub(crate) fn new(job: Job, target: Target, shared: &Shared) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicU32::new(1),
            job: UnsafeCell::new(Some(job)),
            target,
            shared,
        })
    }

    /// One more release to wait for. Only while the creator still holds its share.
    pub(crate) fn retain(&self) {
        self.remaining.fetch_add(1, Ordering::Relaxed);
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
            self.target.push(&*self.shared, job);
        }
    }
}
