use core::sync::atomic::AtomicU32;
use core::time::Duration;

pub(crate) trait Futex {
    /// Blocks while `*addr == expected` until woken, timed out, or woken spuriously. Returns
    /// `false` only on timeout, and only once the whole timeout has passed.
    fn wait(addr: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool;
    fn wake_one(addr: &AtomicU32);
    fn wake_all(addr: &AtomicU32);
}
