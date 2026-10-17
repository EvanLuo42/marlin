//! marlin-config's tests (quick tier, docs/design/0014-testing.md §5): cvars, the console and
//! threads, each test with its own cvars so they share the global registry. Tests that need
//! the whole registry or a whole layer are `isolated_*` binaries. Platform-independent.
//! Run alone with `cargo test -p marlin-config --test unit`.

mod console;
mod cvars;
mod threads;
