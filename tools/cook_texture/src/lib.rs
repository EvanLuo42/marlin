//! Texture cooking: mip generation, block compression and the layouts textures upload in.
//!
//! - [`Image`]: an in-memory source image (8-bit or f32 RGBA) with its color space and usage.
//!   Decoding PNG, EXR and the rest is the importers' job.
//! - [`generate_mips`]: mip chains filtered in linear light (box, Kaiser or Lanczos), any size,
//!   wrapping or clamped edges, renormalized normals and optional alpha-coverage preservation.
//! - [`encode`] / [`encode_texture`]: BC1 (opaque or 1-bit alpha), BC3, BC4, BC5, BC6H (unsigned
//!   and signed) and BC7 (all modes) at three [`Quality`] levels, parallel over block rows.
//!   Every encoder is our own. Output is tightly packed block rows per mip and layer.
//! - [`decode`] / [`decode_with`]: reference decoders for every format, for tests and the
//!   [`metrics`]: the spec's exact arithmetic, or NVIDIA's bit for bit ([`Interpolation`]).
//! - [`EncodedTexture::upload_layout`]: the staging layout marlin-rhi's buffer-to-texture copies
//!   take.
//! - [`TextureTiling`]: the hook console platforms tile textures through.

mod bc;
pub mod color;
mod encode;
mod format;
mod image;
mod layout;
pub mod metrics;
mod mip;
mod par;
mod tiling;

pub use bc::block_mode;
pub use encode::{EncodedTexture, Subresource, decode, decode_with, encode, encode_texture};
pub use format::{EncodeOptions, Format, Interpolation, Quality};
pub use image::{ColorSpace, Image, Pixels, Usage};
pub use layout::{UploadAlignment, UploadLayout, UploadRegion};
pub use mip::{
    EdgeMode, MipFilter, MipOptions, alpha_coverage, generate_mips, mip_level_count, mip_size,
};
pub use tiling::{LinearTiling, TextureTiling, TiledSubresource, TiledTexture, platform_tiling};
