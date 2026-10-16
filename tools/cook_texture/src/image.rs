//! In-memory source images.

use crate::color;

/// How stored color values map to light. Alpha is always linear.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ColorSpace {
    Linear,
    Srgb,
}

/// What an image holds; decides how mips are filtered and which format suits it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Usage {
    Color,
    /// Unit vectors with XYZ remapped from [-1, 1] to [0, 1]. Linear; renormalized per mip.
    Normal,
    /// Data in the red channel (masks, roughness, heights).
    SingleChannel,
    /// Linear radiance in f32, unbounded above.
    Hdr,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Pixels {
    /// 8-bit unorm RGBA.
    U8(Vec<[u8; 4]>),
    F32(Vec<[f32; 4]>),
}

/// A 2D RGBA image, row-major from the top-left texel.
#[derive(Clone, PartialEq, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub color_space: ColorSpace,
    pub usage: Usage,
    pub pixels: Pixels,
}

impl Image {
    /// Panics if `texels` is not `width * height` long or either size is 0.
    pub fn from_u8(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        usage: Usage,
        texels: Vec<[u8; 4]>,
    ) -> Self {
        Self::new(width, height, color_space, usage, Pixels::U8(texels))
    }

    /// Panics like [`from_u8`](Self::from_u8).
    pub fn from_f32(
        width: u32,
        height: u32,
        color_space: ColorSpace,
        usage: Usage,
        texels: Vec<[f32; 4]>,
    ) -> Self {
        Self::new(width, height, color_space, usage, Pixels::F32(texels))
    }

    fn new(width: u32, height: u32, color_space: ColorSpace, usage: Usage, pixels: Pixels) -> Self {
        let len = match &pixels {
            Pixels::U8(t) => t.len(),
            Pixels::F32(t) => t.len(),
        };
        assert!(width > 0 && height > 0, "empty image");
        assert_eq!(len, width as usize * height as usize, "texel count");
        Self {
            width,
            height,
            color_space,
            usage,
            pixels,
        }
    }

    /// Texel values as stored, 8-bit values mapped to [0, 1].
    pub fn to_f32(&self) -> Vec<[f32; 4]> {
        match &self.pixels {
            Pixels::U8(t) => t.iter().map(|t| t.map(|c| c as f32 / 255.0)).collect(),
            Pixels::F32(t) => t.clone(),
        }
    }

    /// Texel values in `space`: sRGB color converted to or from linear light as needed.
    pub fn texels_in(&self, space: ColorSpace) -> Vec<[f32; 4]> {
        match (self.color_space, space, &self.pixels) {
            (ColorSpace::Srgb, ColorSpace::Linear, Pixels::U8(t)) => t
                .iter()
                .map(|t| {
                    let [r, g, b, a] = *t;
                    [
                        color::srgb8_to_linear(r),
                        color::srgb8_to_linear(g),
                        color::srgb8_to_linear(b),
                        a as f32 / 255.0,
                    ]
                })
                .collect(),
            (ColorSpace::Srgb, ColorSpace::Linear, _) => {
                map_rgb(self.to_f32(), color::srgb_to_linear)
            }
            (ColorSpace::Linear, ColorSpace::Srgb, _) => {
                map_rgb(self.to_f32(), |c| color::linear_to_srgb(c.clamp(0.0, 1.0)))
            }
            _ => self.to_f32(),
        }
    }

    /// An image like `self` (size aside) holding `texels`, given in `space`: converted to
    /// `self`'s color space and storage, 8-bit values rounded.
    pub fn with_texels(
        &self,
        width: u32,
        height: u32,
        space: ColorSpace,
        texels: Vec<[f32; 4]>,
    ) -> Self {
        let texels = match (space, self.color_space) {
            (ColorSpace::Linear, ColorSpace::Srgb) => {
                map_rgb(texels, |c| color::linear_to_srgb(c.clamp(0.0, 1.0)))
            }
            (ColorSpace::Srgb, ColorSpace::Linear) => map_rgb(texels, color::srgb_to_linear),
            _ => texels,
        };
        let pixels = match self.pixels {
            Pixels::U8(_) => Pixels::U8(
                texels
                    .iter()
                    .map(|t| t.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
                    .collect(),
            ),
            Pixels::F32(_) => Pixels::F32(texels),
        };
        Self::new(width, height, self.color_space, self.usage, pixels)
    }
}

fn map_rgb(mut texels: Vec<[f32; 4]>, f: impl Fn(f32) -> f32) -> Vec<[f32; 4]> {
    for t in &mut texels {
        for c in &mut t[..3] {
            *c = f(*c);
        }
    }
    texels
}
