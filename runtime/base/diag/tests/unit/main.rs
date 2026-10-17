//! marlin-diag's tests that leave the global logger alone (quick tier, docs/design/0014-testing.md
//! §5): backtraces, buffers and sinks used directly. No hardware needed. Tests of the global
//! registry and filters each own a process: see the `isolated_log_*` binaries.
//! Run alone with `cargo test -p marlin-diag --test unit`.

mod async_sink;
mod backtrace;
mod buf;
mod file_sink;
mod ring_sink;
