//! Glyph rasterization, atlases and text shaping (docs/design/0012-ui-renderer.md §6).
//!
//! - [`asset`]: the `Font` asset and its cooked form ([`FontView`]), read in place.
//! - [`raster`]: outlines to 8-bit coverage at a pixel size and a quarter-pixel horizontal
//!   position.
//! - [`pack`]: skyline bottom-left packing.
//! - [`cache`]: the glyph cache: stable glyph ids, pages evicted whole by LRU, the GPU table.
//! - [`layout`]: U1's minimal text layout (character map and advances).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
pub mod cache;
pub mod layout;
pub mod pack;
pub mod raster;

pub use asset::{CookedFont, Font, FontError, FontView, GlyphEntry, Segment};
pub use cache::{
    FontId, GlyphCache, GlyphCacheConfig, GlyphId, GlyphIds, GlyphKey, GlyphRecord, GlyphUpload,
    Placement, PrepareStats,
};
pub use layout::{PositionedGlyph, TextLayout};
pub use raster::{GlyphBitmap, SUBPIXEL_POSITIONS, rasterize};
