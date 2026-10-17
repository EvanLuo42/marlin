//! marlin-cook-texture's CPU tests (quick tier, docs/design/0014-testing.md §5): color
//! conversions, mips, layouts, encoder quality against the reference decoders, and the cooker.
//! What the GPU decodes is the `gpu` binary.
//! Run alone with `cargo test -p marlin-cook-texture --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod color;
mod cooker;
mod layout;
mod mips;
mod quality;
