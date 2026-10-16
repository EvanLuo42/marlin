//! The cook pipeline's texture cooker: a [`Texture`] asset in, a cooked texture out
//! (`marlin_render::asset::texture`).
//!
//! Decode → (cube: resample the equirectangular source) → mip chain, filtered in linear light →
//! drop levels above `MaxSize` → encode → lay out in the RHI's copy layout, one bulk blob per
//! level above the mip tail and one for the tail.

use marlin_asset::cooked::{BulkKind, Encoding};
use marlin_cook_core::{CookContext, Cooked, Cooker, SourceObject};
use marlin_reflect::type_info;
use marlin_render::asset::texture::{
    AddressMode, CompressionQuality, CookedTexture, MipFilter as SourceMipFilter, PixelFormat,
    Texture, TextureCompression, TextureRegion, TextureShape, TextureUsage,
};
use marlin_serial::cooked::{CookedBuilder, RelSlice};

use crate::color::f32_to_f16;
use crate::cube::equirect_to_cube;
use crate::encode::encode;
use crate::format::{EncodeOptions, Format, Quality};
use crate::image::{ColorSpace, Image, Usage};
use crate::import::{SourceFormat, decode};
use crate::layout::UploadAlignment;
use crate::mip::{EdgeMode, MipFilter, MipOptions, generate_mips};
use crate::tiling::platform_tiling;

/// Levels whose blob would be at most this size share the mip tail blob: smaller pages cost
/// more in IO requests and bookkeeping than they save in memory.
pub const MIP_TAIL_BYTES: u64 = 64 << 10;

/// Cooks [`Texture`] assets.
pub struct TextureCooker;

impl Cooker for TextureCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let texture: &Texture = object
            .value
            .as_ref()
            .downcast()
            .ok_or("the object is not a Texture")?;
        let platform = ctx.platform().to_owned();
        if matches!(platform.as_str(), "ps5" | "xbox" | "switch2") {
            return Err(match platform_tiling(&platform) {
                None => format!(
                    "cooking textures for {platform} needs its tiler: build with the `{platform}` \
                     feature and its platform repository"
                ),
                Some(_) => format!("tiled texture units for {platform} are not supported yet"),
            });
        }
        let path = &texture.source.path;
        if path.is_empty() {
            return Err("`Source` is not set".into());
        }
        let format = SourceFormat::from_path(path)
            .ok_or_else(|| format!("`{path}`: not a PNG, TGA, JPEG, EXR or HDR image"))?;
        let bytes = ctx.read_source(path)?;
        let decoded = decode(&bytes, format).map_err(|e| format!("`{path}`: {e}"))?;
        let has_alpha = decoded.has_alpha();
        let usage = texture.usage;
        if format.is_hdr() && usage != TextureUsage::Hdr {
            ctx.warn(format!(
                "`{path}` is an HDR image but `Usage` is {usage:?}; it is cooked as such"
            ));
        }
        let pixel_format = choose_format(texture)?;
        if has_alpha
            && matches!(
                pixel_format,
                PixelFormat::Bc1Unorm | PixelFormat::Bc1UnormSrgb
            )
            && texture.alpha_cutoff.is_none()
        {
            ctx.warn(format!(
                "`{path}` has alpha, which Bc1 drops without `AlphaCutoff`"
            ));
        }
        let color_space = if usage == TextureUsage::Color && texture.srgb && !format.is_hdr() {
            ColorSpace::Srgb
        } else {
            ColorSpace::Linear
        };
        let mut image = decoded.into_image(color_space, cook_usage(usage));
        if texture.flip_green {
            image = flip_green(&image);
        }

        let faces = match texture.shape {
            TextureShape::Flat => vec![image],
            TextureShape::Cube => {
                let size = (image.width / 4).max(4) / 4 * 4;
                equirect_to_cube(&image, size).into()
            }
        };
        let mip_options = MipOptions {
            filter: match texture.mip_filter {
                SourceMipFilter::Box => MipFilter::Box,
                SourceMipFilter::Kaiser => MipFilter::Kaiser,
                SourceMipFilter::Lanczos3 => MipFilter::Lanczos3,
            },
            edge: match (texture.shape, texture.address) {
                // Cube faces meet other faces at their edges, not themselves.
                (TextureShape::Cube, _) | (_, AddressMode::Clamp) => EdgeMode::Clamp,
                (_, AddressMode::Wrap) => EdgeMode::Wrap,
            },
            alpha_coverage: texture.alpha_cutoff,
            max_levels: (!texture.mip_maps).then_some(1),
        };
        let mut layers: Vec<Vec<Image>> = faces
            .iter()
            .map(|face| {
                if texture.mip_maps {
                    generate_mips(face, &mip_options)
                } else {
                    vec![face.clone()]
                }
            })
            .collect();
        if texture.max_size > 0 {
            let (w, h) = (layers[0][0].width, layers[0][0].height);
            let full = crate::mip::mip_level_count(w, h);
            let drop = (0..full)
                .take_while(|&m| (w >> m).max(h >> m) > texture.max_size)
                .count()
                .min(full as usize - 1);
            let drop = if texture.mip_maps {
                drop.min(layers[0].len() - 1)
            } else {
                drop
            };
            if drop > 0 && !texture.mip_maps {
                // Without a chain to drop levels from, make the one level that fits.
                let options = MipOptions {
                    max_levels: Some(drop as u32 + 1),
                    ..mip_options
                };
                for layer in &mut layers {
                    let chain = generate_mips(&layer[0], &options);
                    *layer = vec![chain[drop].clone()];
                }
            } else {
                for layer in &mut layers {
                    layer.drain(..drop);
                }
            }
        }
        let (width, height) = (layers[0][0].width, layers[0][0].height);
        if pixel_format.block_extent() == 4 && (width % 4 != 0 || height % 4 != 0) {
            return Err(format!(
                "a block-compressed texture needs a size that is a multiple of 4, and this one is \
                 {width}×{height}: resize the source or use `Format = Rgba8`"
            ));
        }

        let options = EncodeOptions {
            quality: match texture.quality {
                CompressionQuality::Fast => Quality::Fast,
                CompressionQuality::Normal => Quality::Normal,
                CompressionQuality::High => Quality::High,
            },
            alpha_cutoff: if matches!(
                pixel_format,
                PixelFormat::Bc1Unorm | PixelFormat::Bc1UnormSrgb
            ) {
                texture.alpha_cutoff
            } else {
                None
            },
            ..EncodeOptions::default()
        };
        let mip_levels = layers[0].len() as u32;
        let layer_count = layers.len() as u32;
        // surfaces[mip][layer]
        let surfaces: Vec<Vec<Vec<u8>>> = (0..mip_levels as usize)
            .map(|mip| {
                layers
                    .iter()
                    .map(|chain| encode_surface(&chain[mip], pixel_format, &options))
                    .collect()
            })
            .collect();

        let (blobs, regions, mip_tail) = lay_out(pixel_format, width, height, &surfaces);
        for blob in blobs {
            ctx.add_bulk(BulkKind::Texture, blob);
        }
        let mut b = CookedBuilder::new();
        let root = b.push(&CookedTexture {
            format: pixel_format as u32,
            width,
            height,
            layers: layer_count,
            mip_levels,
            shape: texture.shape as u32,
            mip_tail,
            address: texture.address as u32,
            regions: RelSlice::EMPTY,
        });
        let slice = b.push_slice(&regions);
        b.link_slice(
            root.field(core::mem::offset_of!(CookedTexture, regions), |t| {
                &t.regions
            }),
            slice,
        )
        .map_err(|e| e.to_string())?;
        Ok(Cooked {
            type_id: type_info::<Texture>().id,
            encoding: Encoding::Cooked,
            payload: b.bytes().to_vec(),
        })
    }
}

fn cook_usage(usage: TextureUsage) -> Usage {
    match usage {
        TextureUsage::Color | TextureUsage::Data => Usage::Color,
        TextureUsage::Normal => Usage::Normal,
        TextureUsage::Mask => Usage::SingleChannel,
        TextureUsage::Hdr => Usage::Hdr,
    }
}

/// The cooked format for `texture`'s settings, or why they do not fit together.
pub fn choose_format(texture: &Texture) -> Result<PixelFormat, String> {
    let usage = texture.usage;
    let srgb = usage == TextureUsage::Color && texture.srgb;
    let pick =
        |linear: PixelFormat, srgb_format: PixelFormat| if srgb { srgb_format } else { linear };
    let format = match texture.format {
        TextureCompression::Auto => match usage {
            TextureUsage::Color | TextureUsage::Data => {
                pick(PixelFormat::Bc7Unorm, PixelFormat::Bc7UnormSrgb)
            }
            TextureUsage::Normal => PixelFormat::Bc5Unorm,
            TextureUsage::Mask => PixelFormat::Bc4Unorm,
            TextureUsage::Hdr => PixelFormat::Bc6hUfloat,
        },
        TextureCompression::Bc1 => pick(PixelFormat::Bc1Unorm, PixelFormat::Bc1UnormSrgb),
        TextureCompression::Bc3 => pick(PixelFormat::Bc3Unorm, PixelFormat::Bc3UnormSrgb),
        TextureCompression::Bc7 => pick(PixelFormat::Bc7Unorm, PixelFormat::Bc7UnormSrgb),
        TextureCompression::Rgba8 => pick(PixelFormat::Rgba8Unorm, PixelFormat::Rgba8UnormSrgb),
        TextureCompression::Bc4 => PixelFormat::Bc4Unorm,
        TextureCompression::Bc5 => PixelFormat::Bc5Unorm,
        TextureCompression::Bc6h => PixelFormat::Bc6hUfloat,
        TextureCompression::Rgba16Float => PixelFormat::Rgba16Float,
    };
    let hdr_format = matches!(format, PixelFormat::Bc6hUfloat | PixelFormat::Rgba16Float);
    if usage == TextureUsage::Hdr && !hdr_format {
        return Err(format!(
            "`Usage = Hdr` needs `Format = Bc6h` or `Rgba16Float`, not {:?}",
            texture.format
        ));
    }
    if format == PixelFormat::Bc6hUfloat && usage != TextureUsage::Hdr {
        return Err("`Format = Bc6h` is for `Usage = Hdr` textures".into());
    }
    if usage == TextureUsage::Normal
        && !matches!(
            format,
            PixelFormat::Bc5Unorm | PixelFormat::Rgba8Unorm | PixelFormat::Bc7Unorm
        )
    {
        return Err(format!(
            "normal maps cook to Bc5 (or Bc7 / Rgba8), not {:?}",
            texture.format
        ));
    }
    Ok(format)
}

fn flip_green(image: &Image) -> Image {
    let texels = image
        .to_f32()
        .into_iter()
        .map(|[r, g, b, a]| [r, 1.0 - g, b, a])
        .collect();
    let space = image.color_space;
    image.with_texels(image.width, image.height, space, texels)
}

/// One mip of one layer, tightly packed rows.
fn encode_surface(image: &Image, format: PixelFormat, options: &EncodeOptions) -> Vec<u8> {
    let bc = |f: Format| encode(image, f, options);
    match format {
        PixelFormat::Rgba8Unorm => image
            .texels_in(ColorSpace::Linear)
            .iter()
            .flat_map(|t| t.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect(),
        PixelFormat::Rgba8UnormSrgb => image
            .texels_in(ColorSpace::Srgb)
            .iter()
            .flat_map(|t| t.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect(),
        PixelFormat::Rgba16Float => image
            .texels_in(ColorSpace::Linear)
            .iter()
            .flat_map(|t| t.map(f32_to_f16))
            .flat_map(u16::to_le_bytes)
            .collect(),
        PixelFormat::Bc1Unorm => bc(Format::Bc1Unorm),
        PixelFormat::Bc1UnormSrgb => bc(Format::Bc1UnormSrgb),
        PixelFormat::Bc3Unorm => bc(Format::Bc3Unorm),
        PixelFormat::Bc3UnormSrgb => bc(Format::Bc3UnormSrgb),
        PixelFormat::Bc4Unorm => bc(Format::Bc4Unorm),
        PixelFormat::Bc5Unorm => bc(Format::Bc5Unorm),
        PixelFormat::Bc6hUfloat => bc(Format::Bc6hUfloat),
        PixelFormat::Bc6hSfloat => bc(Format::Bc6hSfloat),
        PixelFormat::Bc7Unorm => bc(Format::Bc7Unorm),
        PixelFormat::Bc7UnormSrgb => bc(Format::Bc7UnormSrgb),
    }
}

/// Lays `surfaces[mip][layer]` (tightly packed) out in the RHI's copy layout: blobs (one per
/// level above the tail, then the tail), their regions by mip then layer, and the tail's first
/// level.
pub fn lay_out(
    format: PixelFormat,
    width: u32,
    height: u32,
    surfaces: &[Vec<Vec<u8>>],
) -> (Vec<Vec<u8>>, Vec<TextureRegion>, u32) {
    let align = UploadAlignment::RHI;
    let extent = format.block_extent();
    let mips = surfaces.len() as u32;
    let geometry = |mip: u32| {
        let (w, h) = ((width >> mip).max(1), (height >> mip).max(1));
        let row_bytes = w.div_ceil(extent) * format.block_size();
        (w, h, row_bytes, h.div_ceil(extent))
    };
    // A level's blob size if it had its own.
    let level_size = |mip: u32| {
        let (_, _, row_bytes, rows) = geometry(mip);
        let pitch = row_bytes.next_multiple_of(align.row_pitch) as u64;
        let one = (pitch * rows as u64).next_multiple_of(align.offset);
        one * surfaces[mip as usize].len() as u64
    };
    let mip_tail = (0..mips)
        .find(|&m| level_size(m) <= MIP_TAIL_BYTES)
        .unwrap_or(mips);
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let mut regions = Vec::new();
    for mip in 0..mips {
        let blob = mip.min(mip_tail) as usize;
        if blobs.len() <= blob {
            blobs.push(Vec::new());
        }
        let (w, h, row_bytes, rows) = geometry(mip);
        let pitch = row_bytes.next_multiple_of(align.row_pitch);
        for (layer, data) in surfaces[mip as usize].iter().enumerate() {
            let out = &mut blobs[blob];
            let offset = (out.len() as u64).next_multiple_of(align.offset) as usize;
            out.resize(offset + (pitch * rows) as usize, 0);
            for (row, line) in data.chunks_exact(row_bytes as usize).enumerate() {
                out[offset + row * pitch as usize..][..line.len()].copy_from_slice(line);
            }
            regions.push(TextureRegion {
                bulk: blob as u32,
                mip,
                layer: layer as u32,
                offset: offset as u32,
                bytes_per_row: pitch,
                rows,
                width: w.next_multiple_of(extent),
                height: h.next_multiple_of(extent),
            });
        }
    }
    (blobs, regions, mip_tail)
}
