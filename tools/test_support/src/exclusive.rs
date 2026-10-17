use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held by tests that change state the OS keeps per session rather than per thread: they activate
/// a window (the foreground and focus), clip the cursor or inject input. While it is held no other
/// test in the same binary that takes it runs; the guard survives a panicking holder. Keep it to
/// those tests and say at each use what is shared.
pub fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
