//! marlin-reflect-derive's tests (quick tier, docs/design/0014-testing.md §5): what
//! `#[derive(Reflect)]` generates, used through marlin-reflect as users do; the compile-fail
//! cases are doc tests. Platform-independent.
//! Run alone with `cargo test -p marlin-reflect-derive --test unit`.

mod derive;
