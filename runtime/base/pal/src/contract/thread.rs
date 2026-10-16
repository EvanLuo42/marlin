use core::ptr::NonNull;
use core::time::Duration;

use crate::thread::ThreadStart;
use crate::{CpuSet, Result, ThreadPriority};

/// # Safety
///
/// `spawn` must call `(start.entry)(start)` exactly once, on the new thread.
pub(crate) unsafe trait Thread {
    type Handle: Send + Sync;

    /// Starts a thread with `stack_size` bytes of stack (0 = platform default).
    ///
    /// # Safety
    /// `start` stays valid until its entry runs, and the entry may run on another thread.
    unsafe fn spawn(stack_size: usize, start: NonNull<ThreadStart>) -> Result<Self::Handle>;
    fn join(handle: Self::Handle) -> Result<()>;
    fn detach(handle: Self::Handle);

    /// OS thread id of the calling thread, as shown by debuggers and profilers.
    fn current_id() -> u64;
    /// Best effort; truncated to the platform limit (15 bytes on Linux).
    fn set_current_name(name: &str);
    fn sleep(duration: Duration);
    fn yield_now();
    /// Logical CPUs this process may run on.
    fn available_parallelism() -> usize;

    /// Restricts the calling thread to `cpus`, which is non-empty (checked by the caller).
    fn set_current_affinity(cpus: &CpuSet) -> Result<()>;
    fn current_affinity() -> Result<CpuSet>;
    fn set_current_priority(priority: ThreadPriority) -> Result<()>;
}
