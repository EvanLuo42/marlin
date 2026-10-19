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

pub mod blur;
pub mod command;
pub mod damage;
pub mod gradient;
pub mod list;
mod module;
pub mod path;
pub mod reference;
pub mod renderer;
pub mod shape;
pub mod style;

pub use gradient::{Gradient, GradientShape, GradientSpace};
pub use list::{ChunkSlot, Painter, RetainedChunk, UiDrawList};
pub use module::{UI_FONT, UiModule, load_ui_font};
pub use path::{FillRule, Path};
pub use renderer::{
    UI_FORMAT, UiFrameData, UiOutput, UiPages, UiPrepared, UiRenderer, UiRendererDesc, UiShaders,
    UiSurface, UiSurfaceDesc, UiTextureDesc,
};
pub use style::{
    Affine2, Color, Corners, Dash, ExternalImageId, Glow, GroupDesc, ImageColor, ImageSource,
    ImageStyle, LineCap, Paint, Rect, RectStyle, Shadow, ShapeStyle, Stroke, StrokeStyle,
};
