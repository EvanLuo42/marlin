//! Block-compressed formats and encoder settings.

use crate::image::ColorSpace;

/// The block-compressed formats the encoders produce. Names and block layouts match
/// `marlin_rhi::Format` (which lacks `Bc6hSfloat` so far).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Format {
    /// RGB at 4 bpp, with optional 1-bit alpha ([`EncodeOptions::alpha_cutoff`]).
    Bc1Unorm,
    Bc1UnormSrgb,
    /// BC1 color plus a BC4-style alpha block: 8 bpp.
    Bc3Unorm,
    Bc3UnormSrgb,
    /// One channel at 4 bpp.
    Bc4Unorm,
    /// Two channels (tangent-space normal XY) at 8 bpp.
    Bc5Unorm,
    /// HDR RGB, unsigned half floats, 8 bpp.
    Bc6hUfloat,
    /// HDR RGB, signed half floats, 8 bpp.
    Bc6hSfloat,
    /// High-quality RGBA at 8 bpp.
    Bc7Unorm,
    Bc7UnormSrgb,
}

impl Format {
    pub const ALL: [Self; 10] = [
        Self::Bc1Unorm,
        Self::Bc1UnormSrgb,
        Self::Bc3Unorm,
        Self::Bc3UnormSrgb,
        Self::Bc4Unorm,
        Self::Bc5Unorm,
        Self::Bc6hUfloat,
        Self::Bc6hSfloat,
        Self::Bc7Unorm,
        Self::Bc7UnormSrgb,
    ];

    /// Width and height of a block in texels.
    pub const fn block_extent(self) -> u32 {
        4
    }

    /// Bytes per block.
    pub const fn block_size(self) -> u32 {
        match self {
            Self::Bc1Unorm | Self::Bc1UnormSrgb | Self::Bc4Unorm => 8,
            _ => 16,
        }
    }

    pub const fn is_srgb(self) -> bool {
        matches!(
            self,
            Self::Bc1UnormSrgb | Self::Bc3UnormSrgb | Self::Bc7UnormSrgb
        )
    }

    /// The same layout without sRGB decoding.
    pub const fn linear(self) -> Self {
        match self {
            Self::Bc1UnormSrgb => Self::Bc1Unorm,
            Self::Bc3UnormSrgb => Self::Bc3Unorm,
            Self::Bc7UnormSrgb => Self::Bc7Unorm,
            other => other,
        }
    }

    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Bc6hUfloat | Self::Bc6hSfloat)
    }

    /// The space the encoder works in, and [`decode`](crate::decode) returns values in.
    pub const fn color_space(self) -> ColorSpace {
        if self.is_srgb() {
            ColorSpace::Srgb
        } else {
            ColorSpace::Linear
        }
    }

    /// Blocks across and down a `width` × `height` surface.
    pub const fn blocks(self, width: u32, height: u32) -> (u32, u32) {
        (
            width.div_ceil(self.block_extent()),
            height.div_ceil(self.block_extent()),
        )
    }

    /// Bytes of a tightly packed `width` × `height` surface.
    pub const fn surface_size(self, width: u32, height: u32) -> usize {
        let (w, h) = self.blocks(width, height);
        w as usize * h as usize * self.block_size() as usize
    }
}

/// Encoder effort: how many block modes, partitions and endpoint refinements are searched.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Quality {
    Fast,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EncodeOptions {
    pub quality: Quality,
    /// BC1 only: texels with alpha below this become transparent (BC1's 1-bit alpha mode).
    /// `None`: opaque BC1, alpha ignored.
    pub alpha_cutoff: Option<f32>,
    /// Weights of R, G, B and A in the error the encoders minimize.
    pub channel_weights: [f32; 4],
    /// The block modes the encoder may use (bit n allows mode n): BC1 bit 0 four-color, bit 1
    /// three-color; BC3 alpha, BC4 and BC5 bit 0 eight-value, bit 1 six-value; BC6H bit n mode
    /// n + 1 (the spec numbers them 1 to 14); BC7 bit n mode n. Tests use it to reach every
    /// mode; leave it at `u32::MAX` otherwise. Must allow at least one mode.
    pub mode_mask: u32,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: Quality::Normal,
            alpha_cutoff: None,
            channel_weights: [1.0; 4],
            mode_mask: u32::MAX,
        }
    }
}

impl EncodeOptions {
    pub fn with_quality(quality: Quality) -> Self {
        Self {
            quality,
            ..Self::default()
        }
    }
}

/// How the BC1–BC5 reference decoders interpolate. The spec leaves the arithmetic's precision
/// open and GPUs differ, by up to a few 8-bit steps; BC6H and BC7 decode exactly everywhere.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Interpolation {
    /// The spec's formulas in f32 (`(2 a + b) / 3` for BC1, `(6 a + b) / 7` for BC4, ...): the
    /// reference for quality metrics.
    #[default]
    Exact,
    /// NVIDIA's fixed-point arithmetic: bit-exact against NVIDIA hardware (verified on an
    /// RTX 5060 over random blocks).
    Nvidia,
}
