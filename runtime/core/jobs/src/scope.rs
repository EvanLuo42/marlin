//! Scoped jobs: closures that may borrow from the caller, all finished when the scope returns.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::marker::PhantomData;
use core::ops::Range;

use crate::counter::{Counter, Target};
use crate::scheduler::{Job, JobSystem, Priority, Shared};

/// Spawns jobs that may borrow anything outliving the scope. See [`JobSystem::scope`].
pub struct Scope<'scope, 'env: 'scope> {
    shared: &'scope Shared,
    counter: Arc<Counter>,
    // Invariance over both lifetimes, as in `std::thread::Scope`.
    _scope: PhantomData<&'scope mut &'scope ()>,
    _env: PhantomData<&'env mut &'env ()>,
}

impl<'scope> Scope<'scope, '_> {
    /// Queues `f` at normal priority.
    pub fn spawn<F>(&'scope self, f: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        self.spawn_with(Priority::Normal, f);
    }

    pub fn spawn_with<F>(&'scope self, priority: Priority, f: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        self.push(Target::Queue(priority), f);
    }

    /// Queues `f` for the main thread only, as [`JobSystem::spawn_on_main`]. The scope still
    /// waits for it: away from the main thread, it returns only once the main thread has run it.
    pub fn spawn_on_main<F>(&'scope self, f: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        self.push(Target::Main, f);
    }

    fn push<F>(&'scope self, target: Target, f: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        struct Payload<F> {
            f: F,
            counter: Arc<Counter>,
        }

        /// # Safety
        /// `data` is a `Box<Payload<F>>` leaked by `spawn_with`, run once.
        unsafe fn run<F: FnOnce()>(data: *mut ()) {
            // SAFETY: forwarded from the caller.
            let payload = unsafe { Box::from_raw(data.cast::<Payload<F>>()) };
            let Payload { f, counter } = *payload;
            f();
            counter.decrement();
        }

        self.counter.increment();
        let payload = Box::new(Payload {
            f,
            counter: self.counter.clone(),
        });
        // The payload borrows only what outlives 'scope, and the scope waits for every job, so
        // erasing the lifetime is sound.
        let job = Job {
            run: run::<F>,
            data: Box::into_raw(payload).cast(),
        };
        target.push(self.shared, job);
    }
}

impl JobSystem {
    /// Runs `f` with a [`Scope`] for spawning jobs that borrow from the caller, then waits for all
    /// of them. Called from a job, the wait parks the job's fiber instead of blocking a worker.
    pub fn scope<'env, F, R>(&'env self, f: F) -> R
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R,
    {
        let scope = Scope {
            shared: &self.shared,
            counter: Arc::new(Counter::new()),
            _scope: PhantomData,
            _env: PhantomData,
        };
        let result = f(&scope);
        scope.counter.wait();
        result
    }

    /// Calls `f(i)` for every `i` in `range`, in jobs of up to `grain` indices, and waits.
    pub fn parallel_for<F>(&self, range: Range<usize>, grain: usize, f: F)
    where
        F: Fn(usize) + Sync,
    {
        let grain = grain.max(1);
        let f = &f;
        self.scope(|s| {
            let mut start = range.start;
            while start < range.end {
                let end = range.end.min(start + grain);
                s.spawn(move || (start..end).for_each(f));
                start = end;
            }
        });
    }
}
