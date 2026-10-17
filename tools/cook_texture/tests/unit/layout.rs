//! Formats, tight packing, the upload layout marlin-rhi copies from, and the tiling hook.

use marlin_cook_core::{Platform, TextureLayout};
use marlin_cook_texture::{
    EncodeOptions, EncodedTexture, Format, LinearTiling, MipOptions, Quality, TextureTiling,
    TiledSubresource, TiledTexture, UploadAlignment, decode, encode, encode_texture, generate_mips,
    platform_tiling,
};

use crate::support;

fn rhi_format(format: Format) -> Option<marlin_rhi::Format> {
    Some(match format {
        Format::Bc1Unorm => marlin_rhi::Format::Bc1Unorm,
        Format::Bc1UnormSrgb => marlin_rhi::Format::Bc1UnormSrgb,
        Format::Bc3Unorm => marlin_rhi::Format::Bc3Unorm,
        Format::Bc3UnormSrgb => marlin_rhi::Format::Bc3UnormSrgb,
        Format::Bc4Unorm => marlin_rhi::Format::Bc4Unorm,
        Format::Bc5Unorm => marlin_rhi::Format::Bc5Unorm,
        Format::Bc6hUfloat => marlin_rhi::Format::Bc6hUfloat,
        Format::Bc7Unorm => marlin_rhi::Format::Bc7Unorm,
        Format::Bc7UnormSrgb => marlin_rhi::Format::Bc7UnormSrgb,
        Format::Bc6hSfloat => return None,
    })
}

#[test]
fn formats_match_the_rhi() {
    for format in Format::ALL {
        let Some(rhi) = rhi_format(format) else {
            continue;
        };
        assert_eq!(format.block_size(), rhi.block_size(), "{format:?}");
        assert_eq!(format.block_extent(), rhi.block_extent(), "{format:?}");
        assert_eq!(format.is_srgb(), rhi.is_srgb(), "{format:?}");
        assert_eq!(
            rhi_format(format.linear()),
            Some(rhi.linear()),
            "{format:?}"
        );
    }
    assert_eq!(
        UploadAlignment::RHI,
        UploadAlignment {
            row_pitch: marlin_rhi::TEXTURE_ROW_PITCH_ALIGNMENT,
            offset: marlin_rhi::TEXTURE_OFFSET_ALIGNMENT,
        }
    );
}

fn texture(format: Format) -> EncodedTexture {
    let options = MipOptions::default();
    let layers = [support::natural(100, 36, 1), support::gradient(100, 36)]
        .map(|image| generate_mips(&image, &options));
    encode_texture(&layers, format, &EncodeOptions::with_quality(Quality::Fast))
}

#[test]
fn subresources_are_tightly_packed() {
    for format in [Format::Bc1Unorm, Format::Bc7Unorm] {
        let texture = texture(format);
        assert_eq!(texture.mip_levels, 7);
        assert_eq!(texture.array_layers, 2);
        assert_eq!(texture.subresources.len(), 14);
        let mut offset = 0;
        for (i, s) in texture.subresources.iter().enumerate() {
            assert_eq!(
                (s.mip, s.layer),
                (i as u32 % 7, i as u32 / 7),
                "D3D12 subresource order"
            );
            assert_eq!(s.offset, offset);
            assert_eq!(
                s.row_pitch,
                s.width.div_ceil(4) as usize * format.block_size() as usize
            );
            assert_eq!(s.rows, s.height.div_ceil(4));
            assert_eq!(s.size(), format.surface_size(s.width, s.height));
            offset += s.size();
        }
        assert_eq!(texture.data.len(), offset);
    }
}

#[test]
fn upload_layout_follows_the_rhi_rules() {
    for format in [Format::Bc4Unorm, Format::Bc6hUfloat, Format::Bc7UnormSrgb] {
        let texture = texture(format);
        let rhi = rhi_format(format).unwrap();
        let desc = marlin_rhi::TextureDesc {
            array_layers: 2,
            mip_levels: 7,
            ..marlin_rhi::TextureDesc::new_2d(100, 36, rhi, marlin_rhi::TextureUsage::SAMPLED)
        };
        let layout = texture.upload_layout(UploadAlignment::RHI);
        let mut staging = vec![0xcd; layout.size as usize];
        texture.write_upload(&layout, &mut staging);
        let mut end = 0;
        for (s, r) in texture.subresources.iter().zip(&layout.regions) {
            assert_eq!((r.mip, r.layer), (s.mip, s.layer));
            assert_eq!(r.buffer_offset % marlin_rhi::TEXTURE_OFFSET_ALIGNMENT, 0);
            assert!(r.buffer_offset >= end, "regions overlap");
            let width = desc.mip_size(s.mip).0;
            assert_eq!(r.bytes_per_row, marlin_rhi::aligned_row_pitch(rhi, width));
            // Whole blocks, as the RHI's copies require.
            assert_eq!(
                (r.width, r.height),
                (s.width.next_multiple_of(4), s.height.next_multiple_of(4))
            );
            for row in 0..s.rows as usize {
                let at = r.buffer_offset as usize + row * r.bytes_per_row as usize;
                assert_eq!(
                    &staging[at..][..s.row_pitch],
                    &texture.data[s.offset + row * s.row_pitch..][..s.row_pitch]
                );
            }
            end = r.buffer_offset + r.bytes_per_row as u64 * s.rows as u64;
        }
        assert_eq!(layout.size, end);
    }
}

#[test]
fn partial_blocks_round_trip() {
    // Sizes that are not block multiples: edge texels repeat into the padding.
    for (w, h) in [(1, 1), (5, 3), (2, 7), (9, 9)] {
        let image = support::gradient(w, h);
        for format in Format::ALL {
            let data = encode(&image, format, &EncodeOptions::default());
            assert_eq!(data.len(), format.surface_size(w, h));
            let decoded = decode(&data, w, h, format);
            assert_eq!(decoded.len(), (w * h) as usize);
        }
    }
}

/// A stand-in console tiler: reverses each subresource's blocks and pads to 1 KiB.
struct ReverseTiling;

impl TextureTiling for ReverseTiling {
    fn platform(&self) -> &'static str {
        "test"
    }

    fn tile(&self, texture: &EncodedTexture) -> TiledTexture {
        let block = texture.format.block_size() as usize;
        let mut data = Vec::new();
        let mut subresources = Vec::new();
        for s in &texture.subresources {
            let offset = data.len();
            let linear = texture.subresource_data(s.mip, s.layer);
            for b in linear.chunks_exact(block).rev() {
                data.extend_from_slice(b);
            }
            subresources.push(TiledSubresource {
                mip: s.mip,
                layer: s.layer,
                offset,
                size: linear.len(),
            });
            data.resize(data.len().next_multiple_of(1024), 0);
        }
        TiledTexture {
            format: texture.format,
            width: texture.width,
            height: texture.height,
            mip_levels: texture.mip_levels,
            array_layers: texture.array_layers,
            alignment: 1024,
            data,
            subresources,
        }
    }
}

#[test]
fn tiling_hook() {
    let texture = texture(Format::Bc1Unorm);
    let linear = LinearTiling.tile(&texture);
    assert_eq!(linear.data, texture.data);
    for (t, s) in linear.subresources.iter().zip(&texture.subresources) {
        assert_eq!((t.offset, t.size), (s.offset, s.size()));
    }

    let tiler: &dyn TextureTiling = &ReverseTiling;
    let tiled = tiler.tile(&texture);
    assert_eq!(tiled.subresources.len(), texture.subresources.len());
    let s = &tiled.subresources[1];
    assert_eq!(s.offset % tiled.alignment, 0);
    let first = &tiled.data[s.offset..][..8];
    let linear = texture.subresource_data(1, 0);
    assert_eq!(first, &linear[linear.len() - 8..]);

    // Linear targets have no tiler; console tilers exist only with their repositories.
    for platform in Platform::all() {
        if platform.texture_layout == TextureLayout::Linear {
            assert!(
                platform_tiling(platform.name).is_none(),
                "{}",
                platform.name
            );
        }
    }
}
