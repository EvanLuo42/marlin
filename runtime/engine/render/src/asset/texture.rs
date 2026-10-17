//! Textures: the source settings authored in `.masset` text and the cooked form the renderer
//! uploads.
//!
//! A cooked texture is a [`CookedTexture`] payload (a marlin-serial cooked layout) plus bulk
//! blobs in the RHI's buffer-to-texture copy layout (rows padded to 256 bytes, subresources at
//! 512-byte offsets), so each blob is copied to a staging buffer, or read straight into one, and
//! uploaded without touching the texels. Each mip level above the mip tail is its own blob, so
//! streaming can bring levels in and out one at a time; the small levels share one tail blob
//! that stays resident.

use marlin_asset::SourceFile;
use marlin_reflect::Reflect;
use marlin_rhi::{Format, TEXTURE_OFFSET_ALIGNMENT, TEXTURE_ROW_PITCH_ALIGNMENT};
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

/// What a texture holds. Decides filtering, the default format and how the cooker treats it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum TextureUsage {
    /// Color (albedo, emissive, UI): sRGB unless [`Texture::srgb`] is off.
    #[default]
    Color,
    /// Tangent-space normals, XY stored, Z reconstructed. Renormalized per mip.
    Normal,
    /// One linear channel: masks, roughness, heights.
    Mask,
    /// Linear RGBA data that is not color (flow maps, packed material channels).
    Data,
    /// Linear radiance (EXR, Radiance HDR): sky boxes, light probes.
    Hdr,
}

/// The cooked GPU format. `Auto` picks by usage: BC7 for color and data, BC5 for normals, BC4
/// for masks, BC6H for HDR.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum TextureCompression {
    #[default]
    Auto,
    /// RGB at 4 bpp; 1-bit alpha with [`Texture::alpha_cutoff`].
    Bc1,
    /// RGBA at 8 bpp (BC1 color plus a BC4 alpha block).
    Bc3,
    Bc4,
    Bc5,
    Bc6h,
    Bc7,
    /// Uncompressed 8-bit RGBA: pixel art, lookup tables.
    Rgba8,
    /// Uncompressed half-float RGBA.
    Rgba16Float,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum TextureShape {
    #[default]
    Flat,
    /// A cube map, resampled from an equirectangular (latitude-longitude) source.
    Cube,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum MipFilter {
    Box,
    #[default]
    Kaiser,
    Lanczos3,
}

/// How mip filtering treats the image edge; also the sampler the texture is meant for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum AddressMode {
    #[default]
    Wrap,
    Clamp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
pub enum CompressionQuality {
    Fast,
    #[default]
    Normal,
    High,
}

/// A texture asset: a source image and how to cook it.
///
/// ```text
/// Texture
/// {
///     !Uuid = "…"
///     !Name = "RockAlbedo"
///     Source = `source:env/rock_albedo.png`
///     Format = Bc7
/// }
/// ```
#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct Texture {
    pub source: SourceFile,
    pub usage: TextureUsage,
    pub format: TextureCompression,
    pub shape: TextureShape,
    /// Color textures only: the source is sRGB-encoded and sampled with sRGB decoding.
    pub srgb: bool,
    pub mip_maps: bool,
    pub mip_filter: MipFilter,
    pub address: AddressMode,
    /// Alpha-tested textures: keep each mip's alpha coverage at this cutoff, and with BC1 make
    /// texels below it transparent.
    pub alpha_cutoff: Option<f32>,
    /// Downscale (by whole mips) until neither side exceeds this. 0: no limit.
    pub max_size: u32,
    pub quality: CompressionQuality,
    /// Normal maps authored with +Y down (DirectX convention): flip green to the engine's +Y up.
    pub flip_green: bool,
}

impl Default for Texture {
    fn default() -> Self {
        Self {
            source: SourceFile::default(),
            usage: TextureUsage::Color,
            format: TextureCompression::Auto,
            shape: TextureShape::Flat,
            srgb: true,
            mip_maps: true,
            mip_filter: MipFilter::Kaiser,
            address: AddressMode::Wrap,
            alpha_cutoff: None,
            max_size: 0,
            quality: CompressionQuality::Normal,
            flip_green: false,
        }
    }
}

/// Cooked texel formats, by stable code (the numbering is part of the cooked format).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum PixelFormat {
    Rgba8Unorm = 1,
    Rgba8UnormSrgb = 2,
    Rgba16Float = 3,
    Bc1Unorm = 16,
    Bc1UnormSrgb = 17,
    Bc3Unorm = 18,
    Bc3UnormSrgb = 19,
    Bc4Unorm = 20,
    Bc5Unorm = 21,
    Bc6hUfloat = 22,
    Bc6hSfloat = 23,
    Bc7Unorm = 24,
    Bc7UnormSrgb = 25,
}

impl PixelFormat {
    pub const ALL: [Self; 13] = [
        Self::Rgba8Unorm,
        Self::Rgba8UnormSrgb,
        Self::Rgba16Float,
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

    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|f| *f as u32 == code)
    }

    /// The RHI format, if the RHI has it (it lacks signed BC6H so far).
    pub const fn rhi(self) -> Option<Format> {
        Some(match self {
            Self::Rgba8Unorm => Format::Rgba8Unorm,
            Self::Rgba8UnormSrgb => Format::Rgba8UnormSrgb,
            Self::Rgba16Float => Format::Rgba16Float,
            Self::Bc1Unorm => Format::Bc1Unorm,
            Self::Bc1UnormSrgb => Format::Bc1UnormSrgb,
            Self::Bc3Unorm => Format::Bc3Unorm,
            Self::Bc3UnormSrgb => Format::Bc3UnormSrgb,
            Self::Bc4Unorm => Format::Bc4Unorm,
            Self::Bc5Unorm => Format::Bc5Unorm,
            Self::Bc6hUfloat => Format::Bc6hUfloat,
            Self::Bc6hSfloat => return None,
            Self::Bc7Unorm => Format::Bc7Unorm,
            Self::Bc7UnormSrgb => Format::Bc7UnormSrgb,
        })
    }

    /// Texels per block side: 4 for block-compressed formats, else 1.
    pub const fn block_extent(self) -> u32 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Rgba16Float => 1,
            _ => 4,
        }
    }

    /// Bytes per block (per texel when uncompressed).
    pub const fn block_size(self) -> u32 {
        match self {
            Self::Bc1Unorm | Self::Bc1UnormSrgb | Self::Bc4Unorm => 8,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb => 4,
            Self::Rgba16Float => 8,
            _ => 16,
        }
    }
}

/// The payload root of a cooked texture.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CookedTexture {
    /// A [`PixelFormat`] code.
    pub format: u32,
    pub width: u32,
    pub height: u32,
    /// 6 for a cube map (faces +X, -X, +Y, -Y, +Z, -Z), else 1.
    pub layers: u32,
    pub mip_levels: u32,
    /// [`TextureShape`]: 0 flat, 1 cube.
    pub shape: u32,
    /// The first level of the mip tail: levels from here down share the last blob.
    /// `mip_levels` when there is no tail.
    pub mip_tail: u32,
    /// [`AddressMode`]: 0 wrap, 1 clamp. A sampler hint.
    pub address: u32,
    /// One per subresource, ordered by mip, then layer.
    pub regions: RelSlice<TextureRegion>,
}

// SAFETY: eight u32 and a RelSlice (two u32): 40 bytes, align 4, no padding; any bits are valid.
unsafe impl Pod for CookedTexture {}

/// One mip of one layer: where it lies in which blob, as a buffer-to-texture copy wants it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct TextureRegion {
    /// Index among the object's bulk blobs.
    pub bulk: u32,
    pub mip: u32,
    pub layer: u32,
    /// Byte offset in the blob, a multiple of 512.
    pub offset: u32,
    /// A multiple of 256.
    pub bytes_per_row: u32,
    /// Block rows.
    pub rows: u32,
    /// The copy extent: the mip's size rounded up to whole blocks.
    pub width: u32,
    pub height: u32,
}

// SAFETY: eight u32: 32 bytes, no padding; any bits are valid.
unsafe impl Pod for TextureRegion {}

/// Shape and placement errors of a cooked texture, beyond what the layout view checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureError {
    Layout(LayoutError),
    UnknownFormat(u32),
    /// Sizes, counts or regions that do not describe a texture.
    Malformed,
}

impl From<LayoutError> for TextureError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A checked cooked texture.
#[derive(Clone, Copy, Debug)]
pub struct TextureView<'a> {
    pub header: &'a CookedTexture,
    pub format: PixelFormat,
    pub regions: &'a [TextureRegion],
}

impl<'a> TextureView<'a> {
    /// Reads `payload` and checks that every region is a whole subresource of the texture, in
    /// order, inside blob sizes `bulk_sizes` (indexed by [`TextureRegion::bulk`]).
    pub fn new(payload: &'a [u8], bulk_sizes: &[u64]) -> Result<Self, TextureError> {
        let view = CookedView::new(payload);
        let header: &CookedTexture = view.root()?;
        let format = PixelFormat::from_code(header.format)
            .ok_or(TextureError::UnknownFormat(header.format))?;
        let regions = view.slice(&header.regions)?;
        let (w, h, layers, mips) = (
            header.width,
            header.height,
            header.layers,
            header.mip_levels,
        );
        let max_mips = 32 - w.max(h).leading_zeros();
        if w == 0
            || h == 0
            || !(layers == 1 || layers == 6 && header.shape == 1 && w == h)
            || mips == 0
            || mips > max_mips
            || header.mip_tail > mips
            || regions.len() != (mips * layers) as usize
        {
            return Err(TextureError::Malformed);
        }
        let extent = format.block_extent();
        let mut i = 0;
        for mip in 0..mips {
            let (mw, mh) = ((w >> mip).max(1), (h >> mip).max(1));
            for layer in 0..layers {
                let r = &regions[i];
                i += 1;
                let rows = mh.div_ceil(extent);
                let row_bytes = mw.div_ceil(extent) * format.block_size();
                let blob = mip.min(header.mip_tail) as usize;
                let end = r.offset as u64 + r.bytes_per_row as u64 * rows as u64;
                if r.mip != mip
                    || r.layer != layer
                    || r.bulk as usize != blob
                    || r.rows != rows
                    || r.width != mw.next_multiple_of(extent)
                    || r.height != mh.next_multiple_of(extent)
                    || r.bytes_per_row < row_bytes
                    || !r.bytes_per_row.is_multiple_of(TEXTURE_ROW_PITCH_ALIGNMENT)
                    || !(r.offset as u64).is_multiple_of(TEXTURE_OFFSET_ALIGNMENT)
                    || bulk_sizes.get(blob).is_none_or(|&size| end > size)
                {
                    return Err(TextureError::Malformed);
                }
            }
        }
        Ok(Self {
            header,
            format,
            regions,
        })
    }

    /// Bulk blobs the texture has: one per level above the tail, plus the tail.
    pub fn blob_count(&self) -> u32 {
        self.header.mip_tail + u32::from(self.header.mip_tail < self.header.mip_levels)
    }
}
