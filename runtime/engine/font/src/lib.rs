//! Glyph rasterization, atlases and text shaping (docs/design/0012-ui-renderer.md §6).
//!
//! - [`asset`]: the `Font` asset and its cooked form ([`FontView`]), read in place.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;

pub use asset::{CookedFont, Font, FontError, FontView, GlyphEntry, Segment};
