//! marlin-framework's platform-independent tests (quick tier, docs/design/0014-testing.md §5): the
//! boot configuration and the engine's shaders from mounted content.
//! Run alone with `cargo test -p marlin-framework --test unit`.

mod boot;
mod extract;
mod shaders;
