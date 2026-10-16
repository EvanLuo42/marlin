//! OS threads.
//!
//! The PAL does not allocate, so it cannot box a closure. Instead the caller owns a struct whose
//! first field is a [`ThreadStart`]; the new thread calls its `entry` with a pointer to it, and the
//! entry casts back to the full struct and frees it if needed. `marlin-thread` builds the
//! closure-based API on top.

use core::ptr::NonNull;
use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::Thread;
use crate::{CpuSet, Error, ErrorKind, Result, ThreadPriority};

/// Header of a thread's start data. Embed it as the first field of a `#[repr(C)]` struct and
/// derive the pointer from the whole struct, so `entry` may cast back to it.
#[repr(C)]
pub struct ThreadStart {
    /// Runs on the new thread with the pointer passed to [`spawn`].
    pub entry: unsafe fn(NonNull<ThreadStart>),
}

/// Starts a thread with `stack_size` bytes of stack (0 = platform default; rounded up to the
/// platform minimum and page size).
///
/// # Safety
///
/// `start` must stay valid until its entry runs, and the entry must be safe to call from the new
/// thread.
#[inline]
pub unsafe fn spawn(stack_size: usize, start: NonNull<ThreadStart>) -> Result<JoinHandle> {
    // SAFETY: forwarded from the caller.
    unsafe { Backend::spawn(stack_size, start) }.map(|handle| JoinHandle(Some(handle)))
}

/// An owned thread. Dropping it detaches the thread.
#[must_use = "dropping a JoinHandle detaches the thread"]
pub struct JoinHandle(Option<<Backend as Thread>::Handle>);

impl JoinHandle {
    /// Waits for the thread to finish.
    pub fn join(mut self) -> Result<()> {
        Backend::join(self.0.take().unwrap())
    }

    pub fn detach(self) {}
}

impl Drop for JoinHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            Backend::detach(handle);
        }
    }
}

/// OS thread id of the calling thread, as shown by debuggers and profilers.
#[inline]
pub fn current_id() -> u64 {
    Backend::current_id()
}

/// Names the calling thread for debuggers and profilers. Best effort; truncated to the platform
/// limit (15 bytes on Linux).
#[inline]
pub fn set_current_name(name: &str) {
    Backend::set_current_name(name)
}

/// Sleeps for at least `duration`, with the finest resolution the platform offers (100 ns timers
/// on Windows 10 1803+). A zero duration may yield.
#[inline]
pub fn sleep(duration: Duration) {
    Backend::sleep(duration)
}

/// Offers the rest of the time slice to another ready thread.
#[inline]
pub fn yield_now() {
    Backend::yield_now()
}

/// Logical CPUs this process may run on (respects affinity masks on Linux).
#[inline]
pub fn available_parallelism() -> usize {
    Backend::available_parallelism()
}

/// Restricts the calling thread to the CPUs in `cpus` (hard affinity). Ids come from
/// [`cpu_topology`](crate::sysinfo::cpu_topology).
///
/// - Windows: `SetThreadGroupAffinity`. A thread runs in one processor group, so `cpus` must lie
///   in a single group (ids `64 * g ..= 64 * g + 63`); otherwise `InvalidInput`.
/// - Linux: `sched_setaffinity` on the calling thread.
/// - macOS: `Unsupported`; the kernel has no hard affinity. Use [`set_current_priority`] (QoS)
///   to steer work between P and E cores.
///
/// An empty set, or one with no CPU the process may use, is `InvalidInput`.
#[inline]
pub fn set_current_affinity(cpus: &CpuSet) -> Result<()> {
    if cpus.is_empty() {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Backend::set_current_affinity(cpus)
}

/// The CPUs the calling thread may run on. `Unsupported` on macOS.
///
/// On Windows this covers the thread's current processor group only, so on machines with more
/// than 64 CPUs it can be narrower than what a new thread may use by default.
#[inline]
pub fn current_affinity() -> Result<CpuSet> {
    Backend::current_affinity()
}

/// Sets the calling thread's scheduling priority (see [`ThreadPriority`] for the mapping).
///
/// On Linux, lowering the nice value (raising priority above its current level) needs
/// `CAP_SYS_NICE` or a sufficient `RLIMIT_NICE`, else `PermissionDenied`. That includes going back
/// to `Normal` after `Background` or `Low`.
#[inline]
pub fn set_current_priority(priority: ThreadPriority) -> Result<()> {
    Backend::set_current_priority(priority)
}
