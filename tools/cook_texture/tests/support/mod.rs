//! Synthetic test images, and the [`Project`] shared with the other cook crates (`project.rs`
//! from marlin-asset-db's tests, `cook.rs` from marlin-cook-core's).
#![allow(dead_code, unused_imports, reason = "each test binary uses a subset")]

#[path = "../../../asset_db/tests/support/project.rs"]
mod project;

#[path = "../../../cook_core/tests/support/cook.rs"]
mod cook;

pub use project::{Project, uuid};

use marlin_cook_texture::{ColorSpace, Image, Usage};

pub use marlin_math::Rng;

fn u8_image(
    w: u32,
    h: u32,
    space: ColorSpace,
    usage: Usage,
    f: impl Fn(f32, f32) -> [f32; 4],
) -> Image {
    let texels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            f(u, v).map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect();
    Image::from_u8(w, h, space, usage, texels)
}

/// Smooth color ramps: the easy case.
pub fn gradient(w: u32, h: u32) -> Image {
    u8_image(w, h, ColorSpace::Srgb, Usage::Color, |u, v| {
        [u, v, 1.0 - 0.5 * (u + v), 1.0]
    })
}

/// Independent uniform noise in every channel: the worst case.
pub fn noise(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng::new(seed);
    let texels = (0..w * h)
        .map(|_| core::array::from_fn(|_| rng.next_u64() as u8))
        .collect();
    Image::from_u8(w, h, ColorSpace::Srgb, Usage::Color, texels)
}

/// Smooth color with mild noise: closest to photographic content.
pub fn natural(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng::new(seed);
    let n: Vec<[f32; 3]> = (0..w * h)
        .map(|_| core::array::from_fn(|_| rng.f32() - 0.5))
        .collect();
    let texels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let base = [
                0.5 + 0.4 * (6.0 * u + 2.0 * v).sin(),
                0.5 + 0.35 * (4.0 * v - 3.0 * u).cos(),
                0.4 + 0.3 * (5.0 * (u + v)).sin(),
            ];
            let noise = n[(y * w + x) as usize];
            let t: [f32; 3] = core::array::from_fn(|c| base[c] + 0.08 * noise[c]);
            [t[0], t[1], t[2], 1.0].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect();
    Image::from_u8(w, h, ColorSpace::Srgb, Usage::Color, texels)
}

/// Saturated shapes with hard edges: a checkerboard of primaries under a diagonal split.
pub fn edges(w: u32, h: u32) -> Image {
    const COLORS: [[f32; 4]; 6] = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 0.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
    ];
    u8_image(w, h, ColorSpace::Srgb, Usage::Color, |u, v| {
        let cell = ((u * 5.3) as usize + (v * 3.7) as usize * 2) % 6;
        let c = COLORS[cell];
        if u > v { c } else { COLORS[(cell + 3) % 6] }
    })
}

/// A color gradient whose alpha ramps from 0 to 1 across, with a hard-edged cutout.
pub fn alpha_ramp(w: u32, h: u32) -> Image {
    u8_image(w, h, ColorSpace::Srgb, Usage::Color, |u, v| {
        let hole = ((u - 0.7).powi(2) + (v - 0.5).powi(2)) < 0.04;
        [v, 0.5 * (1.0 - v), u, if hole { 0.0 } else { u }]
    })
}

/// Alpha-tested foliage: opaque blobs on transparent, color irrelevant where cut.
pub fn cutout(w: u32, h: u32) -> Image {
    u8_image(w, h, ColorSpace::Srgb, Usage::Color, |u, v| {
        let inside = ((u * 7.0).sin() * (v * 5.0).cos()) > 0.2;
        [
            0.2 + 0.5 * u,
            0.6,
            0.1 + 0.3 * v,
            if inside { 1.0 } else { 0.0 },
        ]
    })
}

/// Unit normals of a bumpy height field, remapped to [0, 1].
pub fn normal_map(w: u32, h: u32) -> Image {
    u8_image(w, h, ColorSpace::Linear, Usage::Normal, |u, v| {
        let tau = core::f32::consts::TAU;
        let dx = 0.6 * (tau * 3.0 * u).cos() + 0.3 * (tau * 7.0 * (u + v)).sin();
        let dy = 0.6 * (tau * 2.0 * v).sin() + 0.3 * (tau * 5.0 * (u - v)).cos();
        let len = (dx * dx + dy * dy + 1.0).sqrt();
        [
            -dx / len * 0.5 + 0.5,
            -dy / len * 0.5 + 0.5,
            1.0 / len * 0.5 + 0.5,
            1.0,
        ]
    })
}

/// One channel (a height or roughness mask): waves, hard steps and grain.
pub fn single_channel(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng::new(seed);
    let grain: Vec<f32> = (0..w * h).map(|_| rng.f32() - 0.5).collect();
    let texels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let step = if (u * 4.0).fract() < 0.5 { 0.2 } else { 0.0 };
            let r = 0.3
                + 0.25 * (13.0 * u + 7.0 * v).sin() * (5.0 * v).cos()
                + step
                + 0.06 * grain[(y * w + x) as usize];
            let r = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
            [r, r, r, 255]
        })
        .collect();
    Image::from_u8(w, h, ColorSpace::Linear, Usage::SingleChannel, texels)
}

/// HDR radiance spanning 2^-8 to 2^10 across, with colored detail and a few hot spots.
pub fn hdr(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng::new(seed);
    let texels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let exposure = 2f32.powf(-8.0 + 18.0 * u);
            let tint = [
                0.6 + 0.4 * (9.0 * v).sin(),
                0.5 + 0.5 * (7.0 * u + 3.0 * v).cos(),
                0.7 + 0.3 * (11.0 * v).cos(),
            ];
            let spot = if rng.f32() < 0.01 { 50.0 } else { 1.0 };
            let n = 1.0 + 0.05 * (rng.f32() - 0.5);
            [
                exposure * tint[0] * spot * n,
                exposure * tint[1] * spot * n,
                exposure * tint[2] * spot * n,
                1.0,
            ]
        })
        .collect();
    Image::from_f32(w, h, ColorSpace::Linear, Usage::Hdr, texels)
}

/// Signed HDR values in about ±1000 (for BC6H's signed format).
pub fn hdr_signed(w: u32, h: u32, seed: u64) -> Image {
    let mut image = hdr(w, h, seed);
    if let marlin_cook_texture::Pixels::F32(texels) = &mut image.pixels {
        for (i, t) in texels.iter_mut().enumerate() {
            let x = i as u32 % w;
            if (x / 8) % 2 == 1 {
                t[0] = -t[0];
            }
            if (i as u32 / w / 4) % 2 == 1 {
                t[2] = -t[2];
            }
        }
    }
    image
}

/// The channels a format stores.
pub fn channels(format: marlin_cook_texture::Format) -> core::ops::Range<usize> {
    use marlin_cook_texture::Format;
    match format {
        Format::Bc4Unorm => 0..1,
        Format::Bc5Unorm => 0..2,
        Format::Bc1Unorm | Format::Bc1UnormSrgb | Format::Bc6hUfloat | Format::Bc6hSfloat => 0..3,
        _ => 0..4,
    }
}

/// Encodes `image` and returns the PSNR of its decoding against the source in the format's
/// space (dB; multi-exposure PSNR for BC6H), and the encoded data.
pub fn psnr(
    image: &Image,
    format: marlin_cook_texture::Format,
    options: &marlin_cook_texture::EncodeOptions,
) -> (f64, Vec<u8>) {
    use marlin_cook_texture::{decode, encode, metrics};
    let data = encode(image, format, options);
    let decoded = decode(&data, image.width, image.height, format);
    let source = image.texels_in(format.color_space());
    let psnr = if format.is_hdr() {
        // The encoder sees half floats: measure against those.
        let source: Vec<[f32; 4]> = source
            .iter()
            .map(|t| {
                let h = |v: f32| {
                    let v = if format == marlin_cook_texture::Format::Bc6hUfloat {
                        v.max(0.0)
                    } else {
                        v
                    };
                    marlin_cook_texture::color::f16_to_f32(marlin_cook_texture::color::f32_to_f16(
                        v,
                    ))
                };
                [h(t[0]), h(t[1]), h(t[2]), 1.0]
            })
            .collect();
        if format == marlin_cook_texture::Format::Bc6hSfloat {
            // Exposure tone mapping clamps negatives; measure magnitudes and signs apart.
            let abs = |t: &Vec<[f32; 4]>| t.iter().map(|t| t.map(f32::abs)).collect::<Vec<_>>();
            metrics::hdr_psnr(&abs(&source), &abs(&decoded))
        } else {
            metrics::hdr_psnr(&source, &decoded)
        }
    } else {
        metrics::psnr(&source, &decoded, channels(format), 1.0)
    };
    (psnr, data)
}
