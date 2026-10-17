//! marlin-sync's tests (quick tier, docs/design/0014-testing.md §5): platform-independent
//! behavior of the locks, events, channels and other primitives, on the host's PAL.
//! Run alone with `cargo test -p marlin-sync --test unit`.

mod barrier;
mod channel;
mod condvar;
mod event;
mod locks;
mod once;
mod reentrant;
mod semaphore;
mod spin;
mod timed;
