//! The calling thread: its id, and settings for threads not started by a [`Builder`](crate::Builder)
//! (chiefly the main thread).

use core::fmt;

use marlin_pal::{CpuSet, Error, ThreadPriority};

/// OS thread id, as shown by debuggers and profilers. Ids can be reused once a thread exits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadId(u64);

impl ThreadId {
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ThreadId({})", self.0)
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// The calling thread's OS id. A system call on Linux; cache it if it is hot.
#[inline]
pub fn id() -> ThreadId {
    ThreadId(marlin_pal::thread::current_id())
}

/// Names the calling thread. Best effort; truncated to the platform limit (15 bytes on Linux).
#[inline]
pub fn set_name(name: &str) {
    marlin_pal::thread::set_current_name(name)
}

/// The calling thread's name as the OS reports it (set by [`set_name`], a
/// [`Builder`](crate::Builder), or anything else that names threads), or `None` if it has none.
/// On Linux unnamed threads report the process name.
pub fn name() -> Option<alloc::string::String> {
    let mut buf = alloc::vec![0u8; 64];
    loop {
        let len = marlin_pal::thread::current_name(&mut buf).ok()?;
        if len <= buf.len() {
            buf.truncate(len);
            return (len > 0)
                .then(|| alloc::string::String::from_utf8(buf).ok())
                .flatten();
        }
        buf.resize(len, 0);
    }
}

/// Sets the calling thread's priority. See [`Builder::priority`](crate::Builder::priority).
#[inline]
pub fn set_priority(priority: ThreadPriority) -> Result<(), Error> {
    marlin_pal::thread::set_current_priority(priority)
}

/// Restricts the calling thread to `cpus`. Unlike [`Builder::affinity`](crate::Builder::affinity),
/// reports `Unsupported` on platforms without hard affinity (macOS).
#[inline]
pub fn set_affinity(cpus: &CpuSet) -> Result<(), Error> {
    marlin_pal::thread::set_current_affinity(cpus)
}

/// The CPUs the calling thread may run on. `Unsupported` on macOS; on Windows only the thread's
/// current processor group.
#[inline]
pub fn affinity() -> Result<CpuSet, Error> {
    marlin_pal::thread::current_affinity()
}
