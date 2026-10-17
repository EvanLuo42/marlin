//! Fiber-based job system and fiber-aware synchronization.
//!
//! Worker threads run a scheduling loop on fibers. A job that waits for other jobs does not block
//! its thread: its fiber is parked, the worker continues on a fresh fiber, and the parked one is
//! resumed (on whichever worker is free) once its wait is over. Deep job graphs therefore never
//! starve the pool, and waits cost a context switch rather than an OS sleep.
//!
//! ```ignore
//! let jobs = JobSystem::new(Config::default())?;
//! let mut results = [0; 4];
//! jobs.scope(|s| {
//!     for (i, r) in results.iter_mut().enumerate() {
//!         s.spawn(move || *r = i * i);
//!     }
//! }); // Every spawned job has finished here.
//! ```
//!
//! Detached jobs ([`JobSystem::spawn`], [`JobSystem::spawn_after`]) return a [`JobHandle`] that
//! may be kept across frames, waited on, or depended on by later jobs: the frame pipeline's links.
//!
//! Jobs must not panic: the engine aborts on panic, and a panic cannot unwind out of a fiber.
//! Code running in jobs must not keep a thread-local's address across a wait, since the job may
//! resume on another thread.
#![cfg_attr(not(test), no_std)]
#![feature(thread_local)]

extern crate alloc;

mod counter;
mod deque;
mod handle;
mod scheduler;
mod scope;

pub use handle::{Batch, JobHandle};
pub use scheduler::{Config, JobSystem, Priority};
pub use scope::Scope;
