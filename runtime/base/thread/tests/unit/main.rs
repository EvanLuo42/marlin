//! marlin-thread's tests (quick tier, docs/design/0014-testing.md §5): spawning, scoped threads
//! and thread-locals, on the host's PAL and CPUs. No other hardware needed.
//! Run alone with `cargo test -p marlin-thread --test unit`.

mod local;
mod scope;
mod spawn;

#[cfg(marlin_platform = "macos")]
mod platform {
    mod macos;
}
