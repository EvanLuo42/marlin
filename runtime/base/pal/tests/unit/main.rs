//! marlin-pal's tests (quick tier, docs/design/0014-testing.md §5): the OS contract every
//! backend meets (files, queues, watching, futexes, threads, fibers, memory, process info), plus
//! each platform's own behavior in `platform/`. Runs on the host OS; no other hardware needed.
//! The crash handler runs in `isolated_crash_dump`.
//! Run alone with `cargo test -p marlin-pal --test unit`.

mod cancel;
mod debug_time;
mod error;
mod ext;
mod fiber;
mod fs;
mod futex;
mod process;
mod stack;
mod thread;
mod tls;
mod topology;
mod vmem;
mod watch;

mod platform {
    #[cfg(marlin_platform = "linux")]
    mod linux;
    #[cfg(marlin_platform = "windows")]
    mod windows;
}
