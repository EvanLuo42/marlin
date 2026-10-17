//! marlin-net over real loopback sockets (quick tier, docs/design/0014-testing.md §5): the blocking
//! streams, NetQueue, and many connections, large transfers and pipelining. OS-contract tests
//! (§2.1 P1): every platform with a socket backend runs them. Run alone with
//! `cargo test -p marlin-net --test unit`.

#[path = "../support/mod.rs"]
mod support;

// Windows has the only socket backend so far.
#[cfg(marlin_platform = "windows")]
mod blocking;
#[cfg(marlin_platform = "windows")]
mod load;
#[cfg(marlin_platform = "windows")]
mod queue;
