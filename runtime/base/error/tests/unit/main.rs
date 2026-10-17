//! marlin-error's tests (quick tier, docs/design/0014-testing.md §5): what `#[derive(Error)]` and
//! `#[derive(ErrorKind)]` generate. No hardware needed.
//! Run alone with `cargo test -p marlin-error --test unit`.

mod derive;
