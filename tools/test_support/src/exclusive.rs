use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held by tests that share state the OS keeps per session rather than per thread: the
/// foreground window, the cursor clip, injected input, a fullscreen window's display. While it
/// is held no other test in the same binary that takes it runs; the guard survives a panicking
/// holder.
pub fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
