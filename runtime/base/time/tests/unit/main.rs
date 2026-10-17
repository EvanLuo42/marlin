//! marlin-time's tests (quick tier, docs/design/0014-testing.md §5): clocks, sleeping, frame
//! timing and calendar conversion, on the host's clock. No hardware needed.
//! Run alone with `cargo test -p marlin-time --test unit`.

mod frame;
mod system;
mod time;
