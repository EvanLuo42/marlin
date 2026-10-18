//! Immediate-mode UI renderer for primitives and text (docs/design/0012-ui-renderer.md).
//!
//! - [`style`]: colors, rectangles and the styles the painter takes.
//! - [`list`]: recording: [`UiDrawList`], [`Painter`], chunks recorded in parallel and
//!   retained across frames.
//! - [`command`]: the GPU command format.
//! - [`shape`]: every command's bounds, distance and color, as the shaders compute them.
//! - [`reference`]: the CPU reference of binning and shading.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod command;
pub mod list;
pub mod reference;
pub mod shape;
pub mod style;

pub use list::{ChunkSlot, Painter, RetainedChunk, UiDrawList};
pub use style::{
    Color, Corners, Dash, ImageColor, ImageSource, ImageStyle, LineCap, Rect, RectStyle,
    ShapeStyle, Stroke, StrokeStyle,
};
