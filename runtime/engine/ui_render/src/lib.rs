//! Immediate-mode UI renderer for primitives and text (docs/design/0012-ui-renderer.md).
//!
//! - [`style`]: colors, rectangles and the styles the painter takes.
//! - [`list`]: recording: [`UiDrawList`], [`Painter`], chunks recorded in parallel and
//!   retained across frames.
//! - [`command`]: the GPU command format.
//! - [`shape`]: every command's bounds, distance and color, as the shaders compute them.
//! - [`reference`]: the CPU reference of binning and shading.
//! - [`renderer`]: the GPU passes on the render graph.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod command;
pub mod list;
pub mod reference;
pub mod renderer;
pub mod shape;
pub mod style;

pub use list::{ChunkSlot, Painter, RetainedChunk, UiDrawList};
pub use renderer::{
    UI_ENTRIES, UI_FORMAT, UI_PUSH_BYTES, UiFrameData, UiOutput, UiRenderer, UiRendererDesc,
    UiShaders,
};
pub use style::{
    Affine2, Color, Corners, Dash, Glow, GroupDesc, ImageColor, ImageSource, ImageStyle, LineCap,
    Rect, RectStyle, Shadow, ShapeStyle, Stroke, StrokeStyle,
};
