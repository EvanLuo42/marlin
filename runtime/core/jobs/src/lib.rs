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
//! - Detached jobs ([`JobSystem::spawn`], [`JobSystem::spawn_after`]) return a [`JobHandle`]
//!   that may be kept across frames, waited on, cancelled, or depended on by later jobs: the
//!   frame pipeline's links. See [`JobHandle`] for cancellation.
//! - [`Graph`]: a frame's jobs and their dependencies, validated once and run every frame.
//! - [`JobSystem::main_thread`] makes the main thread run jobs while it waits, and run the jobs
//!   that must stay on it ([`JobSystem::spawn_on_main`]). See [`main_thread`].
//! - [`sync`]: locks and signals that park the fiber instead of blocking the worker, and may be
//!   held across waits.
//!
//! # Scheduling
//!
//! Workers serve, in order: fibers whose wait is over, the High queue, their own local queue
//! (newest first: depth first), the Normal queue, the oldest job of another worker's local queue,
//! then the Low queue. Normal jobs spawned by jobs go to the spawning worker's local queue, a
//! lock-free work-stealing deque; the rest go to the global queues. The main thread serves
//! main-thread work before all of that.
//!
//! # Running out of fibers
//!
//! Every job parked in a wait holds a fiber. The pool starts with [`Config::fibers`] and grows on
//! demand up to [`Config::max_fibers`]. At that cap a waiting job switches to a fiber whose wait
//! is over if there is one (which needs no new fiber); otherwise it *helps* on its own fiber
//! until its wait is over: it resumes any fiber that becomes ready, and while waiting on jobs
//! (a scope, handle or graph) it runs queued jobs nested on its own stack. A wait on a
//! [`sync`] lock runs no nested jobs and sleeps instead. This cannot deadlock where waits are
//! fork-join (a job waits only for work it spawned or work spawned before it was queued), but
//! it can where a nested job waits for a job that is waiting beneath it on the same stack,
//! which can only finish once the nested one returns; and nesting deepens the stack, which the
//! guard page turns into a crash rather than corruption. The cap is a safety net: size it so
//! that real workloads stay below it ([`JobSystem::fiber_count`] reports the pool's size).
//!
//! # Fibers and threads
//!
//! Jobs must not panic: the engine aborts on panic, and a panic cannot unwind out of a fiber.
//! Code running in jobs must not keep a thread-local's address across a wait, since the job may
//! resume on another thread: read thread-locals through `#[inline(never)]` functions.
#![cfg_attr(not(test), no_std)]
#![feature(thread_local)]

extern crate alloc;

mod counter;
mod deque;
mod graph;
mod handle;
pub mod main_thread;
mod scheduler;
mod scope;
pub mod sync;
mod wait;

pub use graph::{CycleError, Graph, GraphBuilder, NodeId};
pub use handle::{Batch, JobHandle, is_cancelled};
pub use main_thread::MainThread;
pub use scheduler::{Config, JobSystem, Priority};
pub use scope::Scope;
