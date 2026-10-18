//! marlin-framework's platform-independent tests (quick tier, docs/design/0014-testing.md §5): the
//! boot configuration, the engine's shaders and the UI font from mounted content.
//! Run alone with `cargo test -p marlin-framework --test unit`.

mod boot;
mod extract;
mod shaders;
mod ui;
