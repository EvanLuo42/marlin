//! What the painter's methods take: rectangles, colors and styles.

use marlin_math::Vec2;

/// An sRGB-encoded color with straight alpha, 8 bits a channel (0012 §8.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// `0xRRGGBBAA`.
    pub const fn hex(rgba: u32) -> Self {
        Self::rgba(
            (rgba >> 24) as u8,
            (rgba >> 16) as u8,
            (rgba >> 8) as u8,
            rgba as u8,
        )
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// The word the GPU reads: `r | g << 8 | b << 16 | a << 24`.
    pub const fn packed(self) -> u32 {
        self.r as u32 | (self.g as u32) << 8 | (self.b as u32) << 16 | (self.a as u32) << 24
    }

    pub const fn from_packed(w: u32) -> Self {
        Self::rgba(w as u8, (w >> 8) as u8, (w >> 16) as u8, (w >> 24) as u8)
    }
}

/// An axis-aligned rectangle: `min` inclusive, `max` exclusive, logical pixels, y down.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            min: Vec2::new(x0, y0),
            max: Vec2::new(x1, y1),
        }
    }

    pub fn from_min_size(min: Vec2, size: Vec2) -> Self {
        Self {
            min,
            max: min + size,
        }
    }

    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }
}

/// Corner radii: top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Corners {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl Corners {
    pub const ZERO: Self = Self::all(0.0);

    pub const fn all(r: f32) -> Self {
        Self {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }

    pub fn max(&self) -> f32 {
        self.top_left
            .max(self.top_right)
            .max(self.bottom_right)
            .max(self.bottom_left)
    }
}

/// An outline centered on a shape's edge.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stroke {
    pub width: f32,
    pub color: Color,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RectStyle {
    pub fill: Color,
    pub corners: Corners,
    pub stroke: Option<Stroke>,
}

impl RectStyle {
    pub fn fill(fill: Color) -> Self {
        Self {
            fill,
            ..Self::default()
        }
    }

    pub fn rounded(fill: Color, radius: f32) -> Self {
        Self {
            fill,
            corners: Corners::all(radius),
            stroke: None,
        }
    }
}

/// A filled shape with an optional outline: circles and triangles.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ShapeStyle {
    pub fill: Color,
    pub stroke: Option<Stroke>,
}

impl ShapeStyle {
    pub fn fill(fill: Color) -> Self {
        Self { fill, stroke: None }
    }
}

/// How a line ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineCap {
    /// Flat at the end point.
    #[default]
    Butt,
    /// A half disc around the end point.
    Round,
    /// Flat, half the width beyond the end point.
    Square,
}

/// Dashes along a line: `dash` on, `gap` off, the pattern starting `phase` into it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dash {
    pub dash: f32,
    pub gap: f32,
    pub phase: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StrokeStyle {
    pub width: f32,
    pub color: Color,
    pub cap: LineCap,
    /// Dashes are flat-ended whatever the cap.
    pub dash: Option<Dash>,
}

impl StrokeStyle {
    pub fn new(width: f32, color: Color) -> Self {
        Self {
            width,
            color,
            cap: LineCap::Butt,
            dash: None,
        }
    }
}

/// How an image's texels are encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImageColor {
    /// Sampled as linear color (an sRGB texture, decoded by the sampler), straight alpha: the
    /// cooked UI textures.
    #[default]
    LinearStraight,
    /// Sampled as sRGB-encoded values, premultiplied: what the UI renderer itself writes.
    SrgbPremultiplied,
}

/// A texture an image command samples.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ImageSource {
    /// A texture's bindless view index (`ShaderView::index`) and size in texels.
    Texture {
        view: u32,
        width: u32,
        height: u32,
        color: ImageColor,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ImageStyle {
    /// The part of the texture shown, in UV (0–1).
    pub uv: Rect,
    /// Multiplies the texels.
    pub tint: Color,
    pub corners: Corners,
    /// A bindless sampler index; `None` takes the renderer's bilinear clamping one.
    pub sampler: Option<u32>,
}

impl Default for ImageStyle {
    fn default() -> Self {
        Self {
            uv: Rect::new(0.0, 0.0, 1.0, 1.0),
            tint: Color::WHITE,
            corners: Corners::ZERO,
            sampler: None,
        }
    }
}

/// A 2D affine transform: `(x, y)` → `(a x + c y + e, b x + d y + f)`, as `[a, b, c, d, e, f]`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Affine2(pub [f32; 6]);

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2 {
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub const fn translate(x: f32, y: f32) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    /// A rotation by `radians`, clockwise on screen (y down).
    pub fn rotate(radians: f32) -> Self {
        let (s, c) = marlin_math::scalar::sin_cos(radians);
        Self([c, s, -s, c, 0.0, 0.0])
    }

    /// `self`, then `outer`: `outer(self(p))`.
    pub fn then(self, outer: Self) -> Self {
        outer * self
    }
}

impl core::ops::Mul for Affine2 {
    type Output = Self;

    /// `self(o(p))`.
    fn mul(self, o: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [oa, ob, oc, od, oe, of] = o.0;
        Self([
            a * oa + c * ob,
            b * oa + d * ob,
            a * oc + c * od,
            b * oc + d * od,
            a * oe + c * of + e,
            b * oe + d * of + f,
        ])
    }
}

impl Affine2 {
    pub fn apply(&self, p: Vec2) -> Vec2 {
        let [a, b, c, d, e, f] = self.0;
        Vec2::new(a * p.x + c * p.y + e, b * p.x + d * p.y + f)
    }

    pub fn determinant(&self) -> f32 {
        self.0[0] * self.0[3] - self.0[1] * self.0[2]
    }

    /// The inverse; a singular transform gives a transform to nowhere (all zero).
    pub fn inverse(&self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return Self([0.0; 6]);
        }
        let k = 1.0 / det;
        let (ia, ib, ic, id) = (d * k, -b * k, -c * k, a * k);
        Self([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)])
    }

    /// Only translation and a uniform scale (no rotation, skew or mirroring per axis).
    pub fn is_uniform_scale_translate(&self) -> bool {
        let [a, b, c, d, ..] = self.0;
        b == 0.0 && c == 0.0 && a == d && a > 0.0
    }

    /// Axis-aligned: no rotation or skew.
    pub fn is_axis_aligned(&self) -> bool {
        self.0[1] == 0.0 && self.0[2] == 0.0
    }

    /// A rotation and uniform scale (a similarity, possibly mirrored).
    pub fn is_similarity(&self) -> bool {
        let [a, b, c, d, ..] = self.0;
        (a == d && b == -c) || (a == -d && b == c)
    }
}

/// A drop shadow of a group's combined shape (§4.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shadow {
    /// Logical pixels, on screen.
    pub offset: Vec2,
    /// The blur's radius, logical pixels.
    pub blur: f32,
    pub color: Color,
}

/// A glow around a group's combined shape (§4.3): a shadow without an offset, fading
/// exponentially.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Glow {
    pub radius: f32,
    pub color: Color,
}

/// A group of primitives (§2.2): a transform and an opacity for every member, and a shadow and a
/// glow of their combined shape.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GroupDesc {
    /// From the group's space into its parent's, in logical pixels.
    pub transform: Affine2,
    pub opacity: f32,
    pub shadow: Option<Shadow>,
    pub glow: Option<Glow>,
    /// Shadow and glow use the members' bounding box as a rounded rectangle of this radius
    /// instead of their combined shape; groups of more than 16 members always do.
    pub approximate: Option<f32>,
}

impl Default for GroupDesc {
    fn default() -> Self {
        Self {
            transform: Affine2::IDENTITY,
            opacity: 1.0,
            shadow: None,
            glow: None,
            approximate: None,
        }
    }
}
