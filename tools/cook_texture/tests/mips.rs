//! Mip generation: sizes, filtering in linear light, edges, normals and alpha coverage.

mod common;

use marlin_cook_texture::{
    ColorSpace, EdgeMode, Image, MipFilter, MipOptions, Usage, alpha_coverage, generate_mips,
    mip_level_count, mip_size,
};

const FILTERS: [MipFilter; 3] = [MipFilter::Box, MipFilter::Kaiser, MipFilter::Lanczos3];
const EDGES: [EdgeMode; 2] = [EdgeMode::Wrap, EdgeMode::Clamp];

fn options(filter: MipFilter, edge: EdgeMode) -> MipOptions {
    MipOptions {
        filter,
        edge,
        ..MipOptions::default()
    }
}

#[test]
fn chain_sizes_match_the_rhi() {
    let image = common::natural(37, 20, 1);
    let chain = generate_mips(&image, &MipOptions::default());
    assert_eq!(chain.len() as u32, mip_level_count(37, 20));
    let sizes: Vec<(u32, u32)> = chain.iter().map(|i| (i.width, i.height)).collect();
    assert_eq!(sizes, [(37, 20), (18, 10), (9, 5), (4, 2), (2, 1), (1, 1)]);
    let desc = marlin_rhi::TextureDesc::new_2d(
        37,
        20,
        marlin_rhi::Format::Rgba8Unorm,
        marlin_rhi::TextureUsage::SAMPLED,
    );
    for (level, image) in chain.iter().enumerate() {
        assert_eq!((image.width, image.height), desc.mip_size(level as u32));
        assert_eq!((image.width, image.height), mip_size(37, 20, level as u32));
        assert_eq!(image.color_space, ColorSpace::Srgb);
        assert_eq!(image.usage, Usage::Color);
    }
    assert_eq!(chain[0], image);

    let limited = generate_mips(
        &image,
        &MipOptions {
            max_levels: Some(3),
            ..MipOptions::default()
        },
    );
    assert_eq!(limited.len(), 3);
    let one = common::gradient(1, 1);
    assert_eq!(generate_mips(&one, &MipOptions::default()).len(), 1);
    // Tall strips halve only the long side once the short one reaches 1.
    let strip = generate_mips(&common::gradient(1, 8), &MipOptions::default());
    let sizes: Vec<(u32, u32)> = strip.iter().map(|i| (i.width, i.height)).collect();
    assert_eq!(sizes, [(1, 8), (1, 4), (1, 2), (1, 1)]);
}

#[test]
fn constant_images_stay_constant() {
    let color = [0.3f32, 0.6, 0.9, 0.5];
    let image = Image::from_f32(13, 7, ColorSpace::Linear, Usage::Color, vec![color; 13 * 7]);
    for filter in FILTERS {
        for edge in EDGES {
            for level in generate_mips(&image, &options(filter, edge)) {
                for t in level.to_f32() {
                    for c in 0..4 {
                        assert!((t[c] - color[c]).abs() < 1e-5, "{filter:?} {edge:?}: {t:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn box_averages_pairs() {
    let texels: Vec<[f32; 4]> = (0..16).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
    let image = Image::from_f32(4, 4, ColorSpace::Linear, Usage::Hdr, texels);
    let chain = generate_mips(&image, &options(MipFilter::Box, EdgeMode::Clamp));
    let level1: Vec<f32> = chain[1].to_f32().iter().map(|t| t[0]).collect();
    // Each output averages a 2×2 quad: (0 + 1 + 4 + 5) / 4, ...
    assert_eq!(level1, [2.5, 4.5, 10.5, 12.5]);
    assert_eq!(chain[2].to_f32()[0][0], 7.5);
}

#[test]
fn filtering_happens_in_linear_light() {
    // A black and white checkerboard averages to half the light: sRGB 188, not 128.
    let texels = (0..16)
        .map(|i| {
            if (i % 4 + i / 4) % 2 == 0 {
                [0; 4]
            } else {
                [255; 4]
            }
        })
        .collect();
    let image = Image::from_u8(4, 4, ColorSpace::Srgb, Usage::Color, texels);
    for filter in FILTERS {
        let chain = generate_mips(&image, &options(filter, EdgeMode::Wrap));
        let marlin_cook_texture::Pixels::U8(last) = &chain.last().unwrap().pixels else {
            panic!("storage changed");
        };
        assert!(
            (187..=189).contains(&last[0][0]),
            "{filter:?}: {:?}",
            last[0]
        );
        // Alpha is linear already: plain average.
        assert!(
            (127..=128).contains(&last[0][3]),
            "{filter:?}: {:?}",
            last[0]
        );
    }
}

#[test]
fn edge_modes() {
    // A bright left column. Wrapping, it bleeds into the right edge of the next level;
    // clamped, it does not.
    let texels = (0..16 * 16)
        .map(|i| {
            if i % 16 == 0 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            }
        })
        .collect();
    let image = Image::from_f32(16, 16, ColorSpace::Linear, Usage::Color, texels);
    for filter in [MipFilter::Kaiser, MipFilter::Lanczos3] {
        let right = |edge| generate_mips(&image, &options(filter, edge))[1].to_f32()[7][0];
        assert!(right(EdgeMode::Wrap) > 0.01, "{filter:?}");
        assert!(right(EdgeMode::Clamp).abs() < 1e-3, "{filter:?}");
    }
}

#[test]
fn normals_stay_unit_length() {
    let image = common::normal_map(64, 48);
    for filter in FILTERS {
        for level in generate_mips(&image, &options(filter, EdgeMode::Wrap)) {
            assert_eq!(level.usage, Usage::Normal);
            for t in level.to_f32() {
                let n: [f32; 3] = core::array::from_fn(|c| t[c] * 2.0 - 1.0);
                let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                // 8-bit storage limits the precision.
                assert!((length - 1.0).abs() < 0.02, "{filter:?}: {t:?}");
            }
        }
    }
}

#[test]
fn alpha_coverage_is_preserved() {
    let image = common::cutout(64, 64);
    let cutoff = 0.5;
    let target = alpha_coverage(&image.to_f32(), cutoff);
    let kept = generate_mips(
        &image,
        &MipOptions {
            alpha_coverage: Some(cutoff),
            ..MipOptions::default()
        },
    );
    let plain = generate_mips(&image, &MipOptions::default());
    let mut drift = 0.0f32;
    for (level, (kept, plain)) in kept.iter().zip(&plain).enumerate().skip(1).take(4) {
        let kept = alpha_coverage(&kept.to_f32(), cutoff);
        let plain = alpha_coverage(&plain.to_f32(), cutoff);
        // Each level can only hit multiples of 1 / texels.
        let step = 1.0 / (64 >> level) as f32 / (64 >> level) as f32;
        assert!(
            (kept - target).abs() <= 0.02 + step,
            "level {level}: {kept} vs {target}"
        );
        drift = drift.max((plain - target).abs());
    }
    // Without it, coverage drifts further.
    assert!(drift > 0.02, "{drift}");
}

#[test]
fn hdr_stays_non_negative() {
    let image = common::hdr(40, 24, 1);
    for filter in [MipFilter::Kaiser, MipFilter::Lanczos3] {
        for level in generate_mips(&image, &options(filter, EdgeMode::Clamp)) {
            assert!(
                level
                    .to_f32()
                    .iter()
                    .all(|t| t[..3].iter().all(|&c| c >= 0.0))
            );
        }
    }
}
