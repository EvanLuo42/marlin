//! marlin-profile's tests (quick tier, docs/design/0014-testing.md §5) that share a process: in
//! shipping builds, that the API stays and records nothing. The profiler is process-global, so
//! every test that records is an `isolated_*` binary of its own.
//! Run alone with `cargo test -p marlin-profile --test unit`.

#[cfg(marlin_build = "shipping")]
mod shipping;
