//! The texture cooker through the cook pipeline: sources in, checked cooked textures out.

use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use image::{ImageFormat, Rgb32FImage, Rgba32FImage, RgbaImage};
use marlin_asset::cooked::{BulkKind, Encoding, UnitView};
use marlin_compress::Codec;
use marlin_cook_core::{Cookers, Platform, TextureFamily, TextureLayout};
use marlin_cook_texture::{Format, MIP_TAIL_BYTES, TextureCooker, choose_format, decode, metrics};
use marlin_reflect::type_info;
use marlin_render::asset::texture::{PixelFormat, Texture, TextureCompression, TextureView};
use marlin_serial::cooked::CookedBuffer;

use crate::support::{Project, uuid};

/// Hands out distinct object ids.
static NEXT: AtomicU32 = AtomicU32::new(0);

impl Project {
    /// One `Texture` object in `<name>.masset` with the given extra attributes.
    fn texture(&self, name: &str, source: &str, attrs: &str) {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.write(
            &format!("{name}.masset"),
            format!(
                "!Version 1\nTexture {{ !Uuid = \"{}\" !Name = \"T\" Source = `source:{source}` {attrs} }}\n",
                uuid(n.into())
            ),
        );
    }
}

fn cookers() -> Cookers {
    let mut cookers = Cookers::new();
    cookers.register::<Texture>(TextureCooker).unwrap();
    cookers
}

fn png(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_fn(width, height, |x, y| image::Rgba(f(x, y)));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn exr(width: u32, height: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> Vec<u8> {
    let image = Rgba32FImage::from_fn(width, height, |x, y| image::Rgba(f(x, y)));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::OpenExr).unwrap();
    out.into_inner()
}

fn radiance(width: u32, height: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> Vec<u8> {
    let image = Rgb32FImage::from_fn(width, height, |x, y| image::Rgb(f(x, y)));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Hdr).unwrap();
    out.into_inner()
}

/// The single texture in a cooked unit: its view and its blobs.
fn texture(unit: &CookedBuffer) -> (TextureView<'_>, Vec<&[u8]>) {
    let view = UnitView::new(unit.as_slice()).unwrap();
    let object = view.object(0).unwrap();
    assert_eq!(object.type_id, type_info::<Texture>().id.0);
    assert_eq!(object.encoding, Encoding::Cooked);
    let blobs: Vec<&[u8]> = view
        .object_bulk(&object)
        .map(|b| {
            assert_eq!(b.kind, BulkKind::Texture);
            view.bulk_bytes(&b)
        })
        .collect();
    let sizes: Vec<u64> = blobs.iter().map(|b| b.len() as u64).collect();
    let texture = TextureView::new(view.payload(&object), &sizes).unwrap();
    (texture, blobs)
}

/// Level `mip` of `layer`, unpadded.
fn level(view: &TextureView<'_>, blobs: &[&[u8]], mip: u32, layer: u32) -> Vec<u8> {
    let r = view
        .regions
        .iter()
        .find(|r| r.mip == mip && r.layer == layer)
        .unwrap();
    let w = (view.header.width >> mip).max(1);
    let row_bytes = (w.div_ceil(view.format.block_extent()) * view.format.block_size()) as usize;
    let blob = blobs[r.bulk as usize];
    (0..r.rows as usize)
        .flat_map(|row| &blob[r.offset as usize + row * r.bytes_per_row as usize..][..row_bytes])
        .copied()
        .collect()
}

fn gradient(x: u32, y: u32) -> [u8; 4] {
    [
        (x * 255 / 255) as u8,
        (y * 2) as u8,
        ((x + y) / 3) as u8,
        255,
    ]
}

#[test]
fn color_png_cooks_to_bc7_srgb_with_mips_and_a_tail() {
    let p = Project::new("color");
    p.write("rock.png", png(256, 128, gradient));
    p.texture("rock", "rock.png", "");
    let units = p.cook(&cookers()).unwrap();
    let (view, blobs) = texture(&units[0].bytes);
    assert_eq!(view.format, PixelFormat::Bc7UnormSrgb);
    assert_eq!((view.header.width, view.header.height), (256, 128));
    assert_eq!(view.header.mip_levels, 9);
    assert_eq!(view.header.layers, 1);
    // 256×128 BC7 is 32 KiB: everything fits in the tail.
    assert_eq!(view.header.mip_tail, 0);
    assert_eq!(blobs.len(), 1);

    // Level 0 decodes close to the source.
    let data = level(&view, &blobs, 0, 0);
    let decoded = decode(&data, 256, 128, Format::Bc7UnormSrgb);
    let source: Vec<[f32; 4]> = (0..128)
        .flat_map(|y| (0..256).map(move |x| gradient(x, y).map(|c| c as f32 / 255.0)))
        .collect();
    let psnr = metrics::psnr(&decoded, &source, 0..3, 1.0);
    assert!(psnr > 40.0, "PSNR {psnr}");
}

#[test]
fn large_levels_get_their_own_pages() {
    let p = Project::new("pages");
    p.write(
        "big.png",
        png(1024, 1024, |x, y| [x as u8, y as u8, (x ^ y) as u8, 255]),
    );
    p.texture("big", "big.png", "Format = Bc1");
    let units = p.cook(&cookers()).unwrap();
    let (view, blobs) = texture(&units[0].bytes);
    assert_eq!(view.format, PixelFormat::Bc1UnormSrgb);
    // BC1 1024² = 512 KiB, 512² = 128 KiB, 256² = 32 KiB → tail from level 2.
    assert_eq!(view.header.mip_tail, 2);
    assert_eq!(blobs.len(), 3);
    assert!(blobs[2].len() as u64 <= 2 * MIP_TAIL_BYTES);
    for r in view.regions {
        assert_eq!(r.offset % 512, 0);
        assert_eq!(r.bytes_per_row % 256, 0);
    }
}

#[test]
fn usage_picks_the_format() {
    let p = Project::new("usage");
    p.write("n.png", png(64, 64, |_, _| [128, 128, 255, 255]));
    p.write("m.png", png(64, 64, |x, _| [x as u8 * 4, 0, 0, 255]));
    p.write("d.png", png(64, 64, |x, y| [x as u8, y as u8, 7, 9]));
    p.write(
        "h.exr",
        exr(64, 32, |x, _| [x as f32 * 10.0, 1.0, 0.5, 1.0]),
    );
    p.texture("n", "n.png", "Usage = Normal");
    p.texture("m", "m.png", "Usage = Mask");
    p.texture("d", "d.png", "Usage = Data Format = Rgba8 MipMaps = false");
    p.texture("h", "h.exr", "Usage = Hdr");
    let units = p.cook(&cookers()).unwrap();
    let format = |name: &str| {
        let unit = &units.iter().find(|u| u.path == name).unwrap().bytes;
        let (view, blobs) = texture(unit);
        (view.format, level(&view, &blobs, 0, 0))
    };
    assert_eq!(format("n").0, PixelFormat::Bc5Unorm);
    assert_eq!(format("m").0, PixelFormat::Bc4Unorm);
    assert_eq!(format("h").0, PixelFormat::Bc6hUfloat);
    let (f, data) = format("d");
    assert_eq!(f, PixelFormat::Rgba8Unorm, "data is linear");
    assert_eq!(
        &data[..8],
        &[0, 0, 7, 9, 1, 0, 7, 9],
        "uncompressed is exact"
    );
}

#[test]
fn max_size_drops_levels() {
    let p = Project::new("max");
    p.write("t.png", png(512, 256, gradient));
    p.texture("a", "t.png", "MaxSize = 128");
    p.texture("b", "t.png", "MaxSize = 128 MipMaps = false");
    let units = p.cook(&cookers()).unwrap();
    for unit in units.iter().map(|u| &u.bytes) {
        let (view, _) = texture(unit);
        assert_eq!((view.header.width, view.header.height), (128, 64));
    }
    let mips = |name: &str| {
        let unit = &units.iter().find(|u| u.path == name).unwrap().bytes;
        texture(unit).0.header.mip_levels
    };
    assert_eq!((mips("a"), mips("b")), (8, 1));
}

#[test]
fn cube_from_equirect() {
    let p = Project::new("cube");
    // Top half warm, bottom half cold, -Z (the center column) bright.
    p.write(
        "sky.hdr",
        radiance(256, 128, |x, y| {
            let base = if y < 64 {
                [4.0, 1.0, 0.5]
            } else {
                [0.25, 0.5, 2.0]
            };
            if (120..136).contains(&x) && (56..72).contains(&y) {
                [100.0, 100.0, 100.0]
            } else {
                base
            }
        }),
    );
    p.texture(
        "sky",
        "sky.hdr",
        "Usage = Hdr Shape = Cube Format = Rgba16Float",
    );
    let units = p.cook(&cookers()).unwrap();
    let (view, blobs) = texture(&units[0].bytes);
    assert_eq!(view.header.layers, 6);
    assert_eq!((view.header.width, view.header.height), (64, 64));
    let texel = |layer: u32, x: usize, y: usize| -> [f32; 4] {
        let data = level(&view, &blobs, 0, layer);
        let at = (y * 64 + x) * 8;
        std::array::from_fn(|i| {
            let h = u16::from_le_bytes([data[at + i * 2], data[at + i * 2 + 1]]);
            marlin_cook_texture::color::f16_to_f32(h)
        })
    };
    let up = texel(2, 32, 32);
    let down = texel(3, 32, 32);
    let back = texel(5, 32, 32);
    assert!(up[0] > 3.0 && up[2] < 1.0, "+Y is the warm top: {up:?}");
    assert!(
        down[2] > 1.5 && down[0] < 0.5,
        "-Y is the cold bottom: {down:?}"
    );
    assert!(back[0] > 50.0, "-Z looks at the center: {back:?}");
}

#[test]
fn bad_settings_are_errors() {
    let p = Project::new("bad");
    p.write("odd.png", png(30, 17, gradient));
    p.write("ok.png", png(16, 16, gradient));
    p.texture("odd", "odd.png", "");
    p.texture("hdr", "ok.png", "Usage = Hdr Format = Bc7");
    p.texture("missing", "nope.png", "");
    let errors = p.cook(&cookers()).unwrap_err().join("\n");
    assert!(errors.contains("multiple of 4"), "{errors}");
    assert!(errors.contains("`Usage = Hdr` needs"), "{errors}");
    assert!(errors.contains("nope.png"), "{errors}");
}

#[test]
fn bc1_with_alpha_warns() {
    let p = Project::new("warn");
    p.write("a.png", png(16, 16, |x, _| [9, 9, 9, (x * 16) as u8]));
    p.texture("a", "a.png", "Format = Bc1");
    let units = p.cook(&cookers()).unwrap();
    assert!(
        units[0].warnings.iter().any(|w| w.contains("AlphaCutoff")),
        "{:?}",
        units[0].warnings
    );
}

#[test]
fn formats_follow_the_platform() {
    let astc_only = Platform {
        name: "astc-only",
        texture_compression: &[TextureFamily::Astc],
        texture_layout: TextureLayout::Linear,
        head_codec: Codec::Zstd,
        bulk_codec: Codec::Zstd,
        shader_target: "vulkan-1.4",
    };
    let bc_last = Platform {
        texture_compression: &[TextureFamily::Astc, TextureFamily::Bc],
        ..astc_only
    };
    let auto = Texture::default();
    let bc7 = Texture {
        format: TextureCompression::Bc7,
        ..Texture::default()
    };
    let rgba8 = Texture {
        format: TextureCompression::Rgba8,
        ..Texture::default()
    };

    // Auto takes the most preferred family there is an encoder for.
    assert_eq!(
        choose_format(&auto, &bc_last),
        Ok(PixelFormat::Bc7UnormSrgb)
    );
    let error = choose_format(&auto, &astc_only).unwrap_err();
    assert!(
        error.contains("no encoder for astc-only") && error.contains("astc"),
        "{error}"
    );
    // An explicit format the platform cannot sample is refused, not cooked anyway.
    let error = choose_format(&bc7, &astc_only).unwrap_err();
    assert!(error.contains("astc-only does not sample"), "{error}");
    assert_eq!(
        choose_format(&rgba8, &astc_only),
        Ok(PixelFormat::Rgba8UnormSrgb)
    );
}
