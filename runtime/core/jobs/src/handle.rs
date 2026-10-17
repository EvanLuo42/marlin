//! Detached jobs and the handles that outlive them: work that crosses frames.
//!
//! A scope ends with all its jobs done; a frame pipeline cannot work that way. `extract(N)` runs
//! while `render(N - 1)` still does, and `sim(N + 2)` must not start before `render(N)` has
//! consumed frame N's delta. Detached jobs return a [`JobHandle`] that is kept as long as needed:
//! waited on, checked, cancelled, or named as a dependency of later jobs, which are queued only
//! once their dependencies finish (holding no fiber or worker meanwhile).
//!
//! ```ignore
//! let extract = jobs.spawn(move || extract(&world));
//! let render = jobs.spawn_after(&[&extract], Priority::High, move || render());
//! // Next frames: sim(N + 2) starts after render(N).
//! let sim = jobs.spawn_after(&[&renders[n % 2]], Priority::Normal, move || sim());
//! ```

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::Ordering;

use crate::counter::{Continuation, Counter, Target};
use crate::scheduler::{self, Job, JobSystem, Priority, Shared};

/// Work in flight: one detached job, or every job of a [`batch`](JobSystem::batch). Cheap to
/// clone; complete once all of its jobs have finished. A job that waits on a handle parks its
/// fiber; an outside thread blocks.
///
/// # Cancellation
///
/// [`cancel`](Self::cancel) marks a handle cancelled; it cannot be undone. A job counted by a
/// cancelled handle that has not started yet is *skipped*: when it would start, its closure is
/// dropped without being called. A job that has started runs to completion; it can check
/// [`is_cancelled`] and return early. Skipped or not, the job counts as finished, so the handle
/// still completes, and only once every one of its jobs has finished or been skipped. For a
/// [`spawn_after`](JobSystem::spawn_after) job that is once its dependencies are complete: it
/// is skipped where it would have started, not earlier.
///
/// Cancellation propagates to dependents: a job is also skipped if any handle in its `after`
/// list was cancelled by the time it would start, and its own handle then counts as cancelled,
/// so the jobs after it are skipped in turn. A batch is one unit: cancelling its handle (or
/// skipping one of its jobs because of a cancelled dependency) cancels all its jobs that have
/// not started. Cancelling a complete handle still cancels the dependents that have not started.
#[derive(Clone, Default)]
pub struct JobHandle {
    /// `None`: complete from the start.
    pub(crate) counter: Option<Arc<Counter>>,
}

impl JobHandle {
    /// A handle that is already complete (the dependency of the first frame, say).
    pub const fn complete() -> Self {
        Self { counter: None }
    }

    pub fn is_complete(&self) -> bool {
        self.counter.as_ref().is_none_or(|c| c.is_zero())
    }

    /// Waits until the work is complete. From a job, parks its fiber.
    pub fn wait(&self) {
        if let Some(counter) = &self.counter {
            counter.wait();
        }
    }

    /// Skips the handle's jobs that have not started, and the jobs after them. See the
    /// [type docs](Self#cancellation).
    pub fn cancel(&self) {
        if let Some(counter) = &self.counter {
            counter.cancelled.store(true, Ordering::Release);
        }
    }

    /// Whether [`cancel`](Self::cancel) was called, or a job of this handle was skipped because
    /// a dependency was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.counter
            .as_ref()
            .is_some_and(|c| c.cancelled.load(Ordering::Acquire))
    }
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("complete", &self.is_complete())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Whether the detached job (or batch) the caller runs in was cancelled: a started job's
/// cooperative check. False outside detached jobs, including in scoped jobs they spawn.
pub fn is_cancelled() -> bool {
    let counter = scheduler::current_job();
    // SAFETY: a non-null counter is the running detached job's, which holds it alive.
    !counter.is_null() && unsafe { (*counter).cancelled.load(Ordering::Acquire) }
}

/// Queues `f` once every handle in `after` is complete, counted by `counter` and by the system's
/// detached work.
fn submit<F>(shared: &Shared, counter: &Arc<Counter>, after: &[&JobHandle], target: Target, f: F)
where
    F: FnOnce() + Send + 'static,
{
    struct Payload<F> {
        f: F,
        counter: Arc<Counter>,
        /// For cancellation, complete ones included.
        dependencies: Box<[Arc<Counter>]>,
        shared: *const Shared,
    }

    /// # Safety
    /// `data` is a `Box<Payload<F>>` leaked by `submit`, run once.
    unsafe fn run<F: FnOnce()>(data: *mut ()) {
        // SAFETY: forwarded from the caller.
        let payload = unsafe { Box::from_raw(data.cast::<Payload<F>>()) };
        let Payload {
            f,
            counter,
            dependencies,
            shared,
        } = *payload;
        let skip = counter.cancelled.load(Ordering::Acquire)
            || dependencies
                .iter()
                .any(|d| d.cancelled.load(Ordering::Acquire));
        drop(dependencies);
        if skip {
            // Before the decrement, which releases the jobs after this one.
            counter.cancelled.store(true, Ordering::Release);
            drop(f);
        } else {
            scheduler::set_current_job(&*counter);
            f();
        }
        // The handle first: what it releases is detached work too, counted before this ends.
        counter.decrement();
        // SAFETY: the system outlives its detached work, of which this job is part until now.
        unsafe { (*shared).detached.decrement() };
    }

    counter.increment();
    shared.detached.increment();
    let dependencies: Box<[Arc<Counter>]> =
        after.iter().filter_map(|h| h.counter.clone()).collect();
    let all_complete = dependencies.iter().all(|d| d.is_zero());
    let deps: *const [Arc<Counter>] = &*dependencies;
    let job = Job {
        run: run::<F>,
        data: Box::into_raw(Box::new(Payload {
            f,
            counter: counter.clone(),
            dependencies,
            shared,
        }))
        .cast(),
    };
    if all_complete {
        target.push(shared, job);
        return;
    }
    let continuation = Continuation::new(job, target, shared);
    // SAFETY: the slice lives in the payload, which the job frees only once it runs: not before
    // the creator's release below.
    for dependency in unsafe { &*deps } {
        if !dependency.is_zero() {
            continuation.retain();
            dependency.add_continuation(&continuation);
        }
    }
    // The creator's share: released last, so the job cannot start while it is being registered.
    continuation.release();
}

impl JobSystem {
    /// Queues `f` at normal priority and returns its handle. Unlike scoped jobs it may outlive
    /// the caller's frame; dropping the system waits for it.
    pub fn spawn<F>(&self, f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_with(Priority::Normal, f)
    }

    pub fn spawn_with<F>(&self, priority: Priority, f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_after(&[], priority, f)
    }

    /// Queues `f` once every handle in `after` is complete. Until then it takes no fiber and no
    /// worker. Handles of other job systems may be named too.
    pub fn spawn_after<F>(&self, after: &[&JobHandle], priority: Priority, f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.detached(after, Target::Queue(priority), f)
    }

    /// Queues `f` for the main thread only: it runs when the main thread waits or pumps (see
    /// [`JobSystem::main_thread`]), and stays on that thread through its waits. Until a main
    /// thread is registered it waits in the queue; dropping the system runs it on the dropping
    /// thread.
    pub fn spawn_on_main<F>(&self, f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_on_main_after(&[], f)
    }

    /// As [`spawn_on_main`](Self::spawn_on_main), once every handle in `after` is complete.
    pub fn spawn_on_main_after<F>(&self, after: &[&JobHandle], f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        self.detached(after, Target::Main, f)
    }

    fn detached<F>(&self, after: &[&JobHandle], target: Target, f: F) -> JobHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let counter = Arc::new(Counter::new());
        submit(&self.shared, &counter, after, target, f);
        JobHandle {
            counter: Some(counter),
        }
    }

    /// Runs `f` to spawn detached jobs under one handle, returned without waiting: complete once
    /// every job spawned through the [`Batch`] has finished.
    pub fn batch(&self, f: impl FnOnce(&Batch<'_>)) -> JobHandle {
        let batch = Batch {
            shared: &self.shared,
            counter: Arc::new(Counter::new()),
        };
        // Held while `f` spawns, so jobs finishing early cannot complete the handle.
        batch.counter.increment();
        f(&batch);
        batch.counter.decrement();
        JobHandle {
            counter: Some(batch.counter),
        }
    }
}

/// Spawns the detached jobs of one [`JobSystem::batch`].
pub struct Batch<'a> {
    shared: &'a Shared,
    counter: Arc<Counter>,
}

impl Batch<'_> {
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_after(&[], Priority::Normal, f);
    }

    pub fn spawn_with<F>(&self, priority: Priority, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_after(&[], priority, f);
    }

    /// As [`JobSystem::spawn_after`]; the job counts toward the batch's handle.
    pub fn spawn_after<F>(&self, after: &[&JobHandle], priority: Priority, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        submit(
            self.shared,
            &self.counter,
            after,
            Target::Queue(priority),
            f,
        );
    }

    /// As [`JobSystem::spawn_on_main`]; the job counts toward the batch's handle.
    pub fn spawn_on_main<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        submit(self.shared, &self.counter, &[], Target::Main, f);
    }
}
