//! The main thread as a worker.
//!
//! Windowing and graphics APIs often accept calls only from one thread. [`JobSystem::main_thread`]
//! registers the calling thread as the system's main thread; from then on, until the returned
//! [`MainThread`] is dropped:
//!
//! - every wait on the thread (a scope, [`JobHandle::wait`], [`MainThread::run_until`], a
//!   [`sync`](crate::sync) lock) runs jobs instead of blocking: the thread's own stack is parked
//!   as a fiber and the thread runs the scheduler loop on pool fibers until the wait is over;
//! - it alone runs main-thread jobs ([`JobSystem::spawn_on_main`], [`Scope::spawn_on_main`],
//!   main-thread graph nodes), before other work. A main-thread job stays on the main thread
//!   through its waits;
//! - [`MainThread::pump`] runs what is ready without waiting for anything, for the frame loop's
//!   idle moments.
//!
//! The main thread also runs ordinary jobs and resumes ordinary fibers while it waits, so a wait
//! there may last as long as the job it picked up. Normal jobs it spawns (in its jobs or not) go
//! to its own local queue, which the workers steal from.
//!
//! [`Scope::spawn_on_main`]: crate::Scope::spawn_on_main

use core::marker::PhantomData;
use core::sync::atomic::Ordering;

use crate::handle::JobHandle;
use crate::scheduler::{self, JobSystem, Pending, Shared};

/// The registered main thread of a [`JobSystem`]; unregisters on drop. Not `Send`: it belongs
/// to the thread that made it.
pub struct MainThread<'a> {
    shared: &'a Shared,
    _not_send: PhantomData<*const ()>,
}

impl JobSystem {
    /// Registers the calling thread as this system's main thread. See the
    /// [module docs](crate::main_thread).
    ///
    /// # Panics
    /// If the system already has a main thread, the caller is already another system's main
    /// thread, or the caller is a job.
    pub fn main_thread(&self) -> MainThread<'_> {
        assert!(
            !scheduler::on_worker(),
            "marlin-jobs: a job cannot become the main thread"
        );
        self.try_main_thread().expect(
            "marlin-jobs: the system already has a main thread, or this thread is another's",
        )
    }

    pub(crate) fn try_main_thread(&self) -> Option<MainThread<'_>> {
        if !scheduler::current_main().is_null() || scheduler::on_worker() {
            return None;
        }
        // AcqRel: whoever was the main thread before handed over its local queue's owner side.
        self.shared
            .main
            .registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .ok()?;
        scheduler::set_current_main(&*self.shared);
        Some(MainThread {
            shared: &self.shared,
            _not_send: PhantomData,
        })
    }
}

impl MainThread<'_> {
    /// Runs jobs on this thread until `handle` is complete; the same as `handle.wait()` here.
    /// Returns soon after it completes: at most the job running at that moment finishes first.
    pub fn run_until(&self, handle: &JobHandle) {
        handle.wait();
    }

    /// Runs up to `max` jobs (or resumed fibers) that are ready now, main-thread work first, and
    /// returns how many. Never sleeps or waits: the jobs run on pool fibers, so one that waits
    /// is parked (to finish in a later `pump` or wait, if it is a main-thread job) and pumping
    /// goes on. A job it starts does run to its first wait, however long that takes.
    pub fn pump(&self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        let shared: *const Shared = self.shared;
        // SAFETY: this is the registered main thread, on its own stack and not running jobs: a
        // `MainThread` is not `Send`, and jobs cannot reach it to call this.
        unsafe {
            scheduler::enter_main(shared);
            let worker = scheduler::current_worker();
            (*worker).pump = Some(max);
            (*worker).pumped = 0;
            // The scheduler loop on another fiber runs the jobs, and switches back here when
            // done (see `scheduler::end_pump`).
            if let Some(fiber) = self.shared.pop_ready(true) {
                scheduler::count_pumped(worker);
                scheduler::switch_to(fiber, Pending::None);
            } else if let Some(fiber) = self.shared.take_free() {
                scheduler::switch_to(fiber, Pending::None);
            } else {
                // At the fiber cap: run jobs here, nested; a wait then lasts until it is over.
                (*worker).pump = None;
                while (*worker).pumped < max
                    && let Some((job, is_main)) = self.shared.pop_any(true, (*worker).index)
                {
                    (*worker).pumped += 1;
                    scheduler::run_job(job, is_main);
                }
            }
            let done = (*worker).pumped;
            scheduler::leave_main(shared);
            done
        }
    }
}

impl Drop for MainThread<'_> {
    fn drop(&mut self) {
        scheduler::set_current_main(core::ptr::null());
        self.shared.main.registered.store(false, Ordering::Release);
    }
}
