//! marlin-cookd's tests (quick tier, docs/design/0014-testing.md §5): a game's asset server
//! loading from a cookd on a loopback port, and reloading what changes.
//! Run alone with `cargo test -p marlin-cookd --test unit`.

mod dev;
