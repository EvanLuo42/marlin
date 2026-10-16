//! The hook console platforms tile textures through.
//!
//! Desktop GPUs take linear data and tile it themselves during the upload copy. Consoles load
//! textures straight into memory in the GPU's tiled layout, so the cooker tiles them. Each
//! console's tiler lives in its private repository at `platforms/<name>/cook/texture.rs` (see
//! `platforms/README.md`), is compiled in with the matching cargo feature, and exposes
//! `pub fn tiling() -> &'static dyn TextureTiling`. They are `#[rustfmt::skip]` because rustfmt
//! resolves modules whatever their cfg (see marlin-pal's `backend/mod.rs`).

use crate::encode::EncodedTexture;
use crate::format::Format;

#[cfg(feature = "ps5")]
#[path = "../../../platforms/ps5/cook/texture.rs"]
#[rustfmt::skip]
mod ps5;
#[cfg(feature = "switch2")]
#[path = "../../../platforms/switch2/cook/texture.rs"]
#[rustfmt::skip]
mod switch2;
#[cfg(feature = "xbox")]
#[path = "../../../platforms/xbox/cook/texture.rs"]
#[rustfmt::skip]
mod xbox;

/// A tiled texture as the target platform loads it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TiledTexture {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    /// Required alignment of `data` in memory.
    pub alignment: usize,
    pub data: Vec<u8>,
    /// In [`EncodedTexture::subresources`] order. Subresources may share bytes (packed mip tails).
    pub subresources: Vec<TiledSubresource>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TiledSubresource {
    pub mip: u32,
    pub layer: u32,
    pub offset: usize,
    pub size: usize,
}

/// A platform's texture memory layout. It sees the whole texture, since layouts such as packed
/// mip tails span subresources.
pub trait TextureTiling: Sync {
    /// The platform name, as in `platforms/<name>/`.
    fn platform(&self) -> &'static str;

    fn tile(&self, texture: &EncodedTexture) -> TiledTexture;
}

/// Desktop: the linear layout unchanged, uploaded through the RHI's copies.
pub struct LinearTiling;

impl TextureTiling for LinearTiling {
    fn platform(&self) -> &'static str {
        "desktop"
    }

    fn tile(&self, texture: &EncodedTexture) -> TiledTexture {
        TiledTexture {
            format: texture.format,
            width: texture.width,
            height: texture.height,
            mip_levels: texture.mip_levels,
            array_layers: texture.array_layers,
            alignment: 1,
            data: texture.data.clone(),
            subresources: texture
                .subresources
                .iter()
                .map(|s| TiledSubresource {
                    mip: s.mip,
                    layer: s.layer,
                    offset: s.offset,
                    size: s.size(),
                })
                .collect(),
        }
    }
}

/// The tiler for `platform`, or `None` where textures stay linear (desktop) or the platform's
/// repository is not built in.
pub fn platform_tiling(platform: &str) -> Option<&'static dyn TextureTiling> {
    match platform {
        #[cfg(feature = "ps5")]
        "ps5" => Some(ps5::tiling()),
        #[cfg(feature = "xbox")]
        "xbox" => Some(xbox::tiling()),
        #[cfg(feature = "switch2")]
        "switch2" => Some(switch2::tiling()),
        _ => None,
    }
}
