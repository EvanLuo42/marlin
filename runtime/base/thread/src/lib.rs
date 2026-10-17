//! Threads, fibers, CPU topology and thread-local storage.
//!
//! The safe layer over `marlin_pal::thread`: closure-based [`spawn`] and [`Builder`] (name, stack
//! size, affinity, priority), [`JoinHandle::join`] returning the closure's value, [`scope()`] for
//! threads that borrow from the caller's stack, the calling thread's settings in [`current`], CPU
//! topology, and [`LocalKey`].
//!
//! Panics abort the process, so `join` has no error case for them.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod builder;
pub mod current;
pub mod ext;
pub mod fiber;
mod local;
mod scope;

use core::time::Duration;

pub use builder::{Builder, JoinHandle};
pub use current::ThreadId;
pub use local::LocalKey;
pub use marlin_pal::sysinfo::{LogicalCpu, cpu_topology};
pub use marlin_pal::{CpuSet, Error, ErrorKind, ThreadPriority};
pub use scope::{Scope, ScopedJoinHandle, scope};

/// Starts a thread running `f`, with default settings.
///
/// # Panics
/// If the OS cannot create the thread. Use [`Builder::spawn`] to handle that.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match Builder::new().spawn(f) {
        Ok(handle) => handle,
        Err(error) => panic!("failed to spawn thread: {error}"),
    }
}

/// Sleeps for at least `duration`, with the finest resolution the platform offers. A zero
/// duration may yield.
#[inline]
pub fn sleep(duration: Duration) {
    marlin_pal::thread::sleep(duration)
}

/// Offers the rest of the time slice to another ready thread.
#[inline]
pub fn yield_now() {
    marlin_pal::thread::yield_now()
}

/// Logical CPUs this process may run on (respects affinity masks on Linux).
#[inline]
pub fn available_parallelism() -> usize {
    marlin_pal::thread::available_parallelism()
}
