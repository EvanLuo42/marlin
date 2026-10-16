//! Decoding source images: PNG, TGA, JPEG, OpenEXR and Radiance HDR.
//!
//! Parsing is the `image` crate's (third-party parsers for external formats, per 0003); what
//! comes out is ours: 8-bit sources stay 8-bit, deeper and float sources become f32.

use ::image::{DynamicImage, ImageFormat};

use crate::image::{ColorSpace, Image, Usage};

/// The source formats, by file extension.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SourceFormat {
    Png,
    Tga,
    Jpeg,
    Exr,
    Hdr,
}

impl SourceFormat {
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "png" => Self::Png,
            "tga" => Self::Tga,
            "jpg" | "jpeg" => Self::Jpeg,
            "exr" => Self::Exr,
            "hdr" => Self::Hdr,
            _ => return None,
        })
    }

    /// Whether the format stores linear floating-point radiance.
    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Exr | Self::Hdr)
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Tga => ImageFormat::Tga,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Exr => ImageFormat::OpenExr,
            Self::Hdr => ImageFormat::Hdr,
        }
    }
}

/// A decoded source: RGBA, top-left first. Gray sources are replicated to RGB, missing alpha is
/// opaque.
#[derive(Clone, PartialEq, Debug)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub pixels: DecodedPixels,
}

#[derive(Clone, PartialEq, Debug)]
pub enum DecodedPixels {
    U8(Vec<[u8; 4]>),
    /// 16-bit integer sources mapped to [0, 1], and float sources as stored.
    F32(Vec<[f32; 4]>),
}

impl Decoded {
    /// Whether any texel is not fully opaque.
    pub fn has_alpha(&self) -> bool {
        match &self.pixels {
            DecodedPixels::U8(p) => p.iter().any(|t| t[3] != 255),
            DecodedPixels::F32(p) => p.iter().any(|t| t[3] < 1.0),
        }
    }

    /// As a cook image with the given interpretation.
    pub fn into_image(self, color_space: ColorSpace, usage: Usage) -> Image {
        match self.pixels {
            DecodedPixels::U8(p) => Image::from_u8(self.width, self.height, color_space, usage, p),
            DecodedPixels::F32(p) => {
                Image::from_f32(self.width, self.height, color_space, usage, p)
            }
        }
    }
}

/// Decodes `bytes` as `format`.
pub fn decode(bytes: &[u8], format: SourceFormat) -> Result<Decoded, String> {
    let image = ::image::load_from_memory_with_format(bytes, format.image_format())
        .map_err(|e| format!("decoding {format:?}: {e}"))?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err("the image is empty".into());
    }
    let pixels = match image {
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageRgb8(_)
        | DynamicImage::ImageRgba8(_) => {
            DecodedPixels::U8(image.into_rgba8().pixels().map(|p| p.0).collect())
        }
        other => DecodedPixels::F32(other.into_rgba32f().pixels().map(|p| p.0).collect()),
    };
    Ok(Decoded {
        width,
        height,
        pixels,
    })
}
