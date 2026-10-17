//! marlin-cooker's tests (quick tier, docs/design/0014-testing.md §5): the `marlin-cook` command
//! line, the engine's cooker set, and two projects sharing a store on a loopback port.
//! Run alone with `cargo test -p marlin-cooker --test unit`.

mod cli;
mod engine_cookers;
mod shared;
