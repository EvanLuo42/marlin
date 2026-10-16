//! Whole surfaces and textures.

use crate::bc;
use crate::format::{EncodeOptions, Format, Interpolation};
use crate::image::Image;
use crate::mip::mip_size;
use crate::par;

/// Encodes `image` into tightly packed blocks: block rows top to bottom, no padding. The image
/// is first converted into the format's color space (sRGB formats encode sRGB values, the others
/// linear); partial edge blocks repeat the edge texels.
pub fn encode(image: &Image, format: Format, options: &EncodeOptions) -> Vec<u8> {
    let mut texels = image.texels_in(format.color_space());
    if !format.is_hdr() {
        for t in &mut texels {
            *t = t.map(|c| c.clamp(0.0, 1.0) * 255.0);
        }
    }
    let (w, h) = (image.width as usize, image.height as usize);
    let (blocks_x, _) = format.blocks(image.width, image.height);
    let size = format.block_size() as usize;
    let mut out = vec![0; format.surface_size(image.width, image.height)];
    par::for_each_row(&mut out, blocks_x as usize * size, |by, row| {
        for (bx, block) in row.chunks_exact_mut(size).enumerate() {
            let block_texels = core::array::from_fn(|i| {
                let x = (bx * 4 + i % 4).min(w - 1);
                let y = (by * 4 + i / 4).min(h - 1);
                texels[y * w + x]
            });
            bc::encode_block(format, &block_texels, options, block);
        }
    });
    out
}

/// Decodes tightly packed blocks with the reference decoders, interpolating exactly as the spec
/// writes it: what a shader reads through a view of `format.linear()` (sRGB formats return
/// sRGB-encoded values). Row-major texels.
pub fn decode(data: &[u8], width: u32, height: u32, format: Format) -> Vec<[f32; 4]> {
    decode_with(data, width, height, format, Interpolation::Exact)
}

/// [`decode`] with BC1–BC5 interpolated as `interpolation` says.
pub fn decode_with(
    data: &[u8],
    width: u32,
    height: u32,
    format: Format,
    interpolation: Interpolation,
) -> Vec<[f32; 4]> {
    assert_eq!(data.len(), format.surface_size(width, height), "data size");
    let (w, h) = (width as usize, height as usize);
    let (blocks_x, _) = format.blocks(width, height);
    let mut out = vec![[0.0; 4]; w * h];
    for (b, block) in data.chunks_exact(format.block_size() as usize).enumerate() {
        let (bx, by) = (b % blocks_x as usize, b / blocks_x as usize);
        let texels = bc::decode_block(format, block, interpolation);
        for (i, t) in texels.iter().enumerate() {
            let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
            if x < w && y < h {
                out[y * w + x] = *t;
            }
        }
    }
    out
}

/// One mip of one layer within [`EncodedTexture::data`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Subresource {
    pub mip: u32,
    pub layer: u32,
    /// In texels.
    pub width: u32,
    pub height: u32,
    pub offset: usize,
    /// Bytes per block row: blocks across × block size.
    pub row_pitch: usize,
    /// Block rows.
    pub rows: u32,
}

impl Subresource {
    pub const fn size(&self) -> usize {
        self.row_pitch * self.rows as usize
    }
}

/// Every mip of every layer, tightly packed one after another in D3D12 subresource order (layer
/// by layer, each layer's mips from level 0).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EncodedTexture {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub data: Vec<u8>,
    /// Indexed by `layer * mip_levels + mip`.
    pub subresources: Vec<Subresource>,
}

impl EncodedTexture {
    pub fn subresource(&self, mip: u32, layer: u32) -> &Subresource {
        assert!(mip < self.mip_levels && layer < self.array_layers);
        &self.subresources[(layer * self.mip_levels + mip) as usize]
    }

    pub fn subresource_data(&self, mip: u32, layer: u32) -> &[u8] {
        let s = self.subresource(mip, layer);
        &self.data[s.offset..][..s.size()]
    }
}

/// Encodes every level of every layer. `layers[l]` is layer `l`'s mip chain from level 0 (see
/// [`generate_mips`](crate::generate_mips)); every layer has the same level count and sizes,
/// level `n` sized as [`mip_size`] says.
pub fn encode_texture(
    layers: &[Vec<Image>],
    format: Format,
    options: &EncodeOptions,
) -> EncodedTexture {
    assert!(!layers.is_empty() && !layers[0].is_empty(), "no images");
    let (width, height) = (layers[0][0].width, layers[0][0].height);
    let mip_levels = layers[0].len() as u32;
    let mut data = Vec::new();
    let mut subresources = Vec::new();
    for (layer, chain) in layers.iter().enumerate() {
        assert_eq!(chain.len() as u32, mip_levels, "layer {layer} level count");
        for (mip, image) in chain.iter().enumerate() {
            let expected = mip_size(width, height, mip as u32);
            assert_eq!(
                (image.width, image.height),
                expected,
                "layer {layer} mip {mip} size"
            );
            let (blocks_x, blocks_y) = format.blocks(image.width, image.height);
            subresources.push(Subresource {
                mip: mip as u32,
                layer: layer as u32,
                width: image.width,
                height: image.height,
                offset: data.len(),
                row_pitch: blocks_x as usize * format.block_size() as usize,
                rows: blocks_y,
            });
            data.extend(encode(image, format, options));
        }
    }
    EncodedTexture {
        format,
        width,
        height,
        mip_levels,
        array_layers: layers.len() as u32,
        data,
        subresources,
    }
}
