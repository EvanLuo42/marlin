//! marlin-cook-mesh's tests (quick tier, docs/design/0014-testing.md §5): the mesh algorithms on
//! synthetic geometry, and meshes, skeletons and clips cooked from generated glTF files.
//! Run alone with `cargo test -p marlin-cook-mesh --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod algorithms;
mod clusters;
mod cooker;
