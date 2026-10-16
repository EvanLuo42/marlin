//! Detached jobs and the handles that outlive them: work that crosses frames.
//!
//! A scope ends with all its jobs done; a frame pipeline cannot work that way. `extract(N)` runs
//! while `render(N - 1)` still does, and `sim(N + 2)` must not start before `render(N)` has
//! consumed frame N's delta. Detached jobs return a [`JobHandle`] that is kept as long as needed:
//! waited on, checked, or named as a dependency of later jobs, which are queued only once their
//! dependencies finish (holding no fiber or worker meanwhile).
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

use crate::counter::{Continuation, Counter};
use crate::scheduler::{Job, JobSystem, Priority, Shared};

/// Work in flight: one detached job, or every job of a [`batch`](JobSystem::batch). Cheap to
/// clone; complete once all of its jobs have finished. A job that waits on a handle parks its
/// fiber; an outside thread blocks.
#[derive(Clone, Default)]
pub struct JobHandle {
    /// `None`: complete from the start.
    counter: Option<Arc<Counter>>,
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
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("complete", &self.is_complete())
            .finish()
    }
}

/// Queues `f` once every handle in `after` is complete, counted by `counter` and by the system's
/// detached work.
fn submit<F>(
    shared: &Shared,
    counter: &Arc<Counter>,
    after: &[&JobHandle],
    priority: Priority,
    f: F,
) where
    F: FnOnce() + Send + 'static,
{
    struct Payload<F> {
        f: F,
        counter: Arc<Counter>,
        shared: *const Shared,
    }

    /// # Safety
    /// `data` is a `Box<Payload<F>>` leaked by `submit`, run once.
    unsafe fn run<F: FnOnce()>(data: *mut ()) {
        // SAFETY: forwarded from the caller.
        let payload = unsafe { Box::from_raw(data.cast::<Payload<F>>()) };
        let Payload { f, counter, shared } = *payload;
        f();
        // The handle first: what it releases is detached work too, counted before this ends.
        counter.decrement();
        // SAFETY: the system outlives its detached work, of which this job is part until now.
        unsafe { (*shared).detached.decrement() };
    }

    counter.increment();
    shared.detached.increment();
    let job = Job {
        run: run::<F>,
        data: Box::into_raw(Box::new(Payload {
            f,
            counter: counter.clone(),
            shared,
        }))
        .cast(),
    };
    let pending: alloc::vec::Vec<&Arc<Counter>> = after
        .iter()
        .filter_map(|h| h.counter.as_ref())
        .filter(|c| !c.is_zero())
        .collect();
    if pending.is_empty() {
        shared.push_job(job, priority);
        return;
    }
    let continuation = Continuation::new(pending.len() as u32, job, priority, shared);
    for dependency in pending {
        dependency.add_continuation(&continuation);
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
        let counter = Arc::new(Counter::new());
        submit(&self.shared, &counter, after, priority, f);
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
        submit(self.shared, &self.counter, after, priority, f);
    }
}
