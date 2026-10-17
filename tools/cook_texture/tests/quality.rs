//! Encoder quality on synthetic images: PSNR floors per format and quality level, every block
//! mode round-tripping through the reference decoder, and determinism.

mod common;

use marlin_cook_texture::{
    EncodeOptions, Format, Image, Quality, block_mode, decode, encode, metrics,
};

const SIZE: u32 = 64;
const QUALITIES: [Quality; 3] = [Quality::Fast, Quality::Normal, Quality::High];

/// A format, an image and the PSNR (dB) each quality must reach: measured, less about 0.5 dB.
struct Case {
    format: Format,
    image: &'static str,
    floors: [f64; 3],
}

const fn case(format: Format, image: &'static str, floors: [f64; 3]) -> Case {
    Case {
        format,
        image,
        floors,
    }
}

const CASES: &[Case] = &[
    case(Format::Bc1UnormSrgb, "gradient", [38.0, 38.0, 38.1]),
    case(Format::Bc1UnormSrgb, "natural", [32.1, 32.1, 32.2]),
    case(Format::Bc1UnormSrgb, "edges", [34.2, 34.2, 34.2]),
    case(Format::Bc1UnormSrgb, "noise", [13.3, 13.3, 13.3]),
    case(Format::Bc3UnormSrgb, "alpha_ramp", [38.5, 38.6, 39.1]),
    case(Format::Bc3UnormSrgb, "natural", [33.3, 33.3, 33.5]),
    case(Format::Bc3UnormSrgb, "cutout", [43.9, 46.0, 46.0]),
    case(Format::Bc4Unorm, "single", [45.9, 46.4, 46.6]),
    case(Format::Bc4Unorm, "gradient", [53.5, 53.9, 62.6]),
    case(Format::Bc5Unorm, "normals", [40.3, 40.6, 40.9]),
    case(Format::Bc5Unorm, "noise", [30.1, 30.5, 30.7]),
    case(Format::Bc6hUfloat, "hdr", [35.4, 40.6, 40.9]),
    case(Format::Bc6hSfloat, "hdr_signed", [36.3, 40.2, 40.4]),
    case(Format::Bc7UnormSrgb, "gradient", [46.4, 46.4, 46.5]),
    case(Format::Bc7UnormSrgb, "natural", [37.2, 37.7, 38.1]),
    case(Format::Bc7UnormSrgb, "edges", [55.4, 60.0, 60.0]),
    case(Format::Bc7UnormSrgb, "alpha_ramp", [41.9, 46.1, 46.1]),
    case(Format::Bc7UnormSrgb, "noise", [14.9, 15.2, 15.2]),
];

fn image(name: &str) -> Image {
    match name {
        "gradient" => common::gradient(SIZE, SIZE),
        "natural" => common::natural(SIZE, SIZE, 1),
        "edges" => common::edges(SIZE, SIZE),
        "noise" => common::noise(SIZE, SIZE, 2),
        "alpha_ramp" => common::alpha_ramp(SIZE, SIZE),
        "cutout" => common::cutout(SIZE, SIZE),
        "single" => common::single_channel(SIZE, SIZE, 3),
        "normals" => common::normal_map(SIZE, SIZE),
        "hdr" => common::hdr(SIZE, SIZE, 4),
        "hdr_signed" => common::hdr_signed(SIZE, SIZE, 5),
        _ => unreachable!(),
    }
}

#[test]
fn psnr_floors() {
    let mut failures = Vec::new();
    for case in CASES {
        let image = image(case.image);
        let psnr =
            QUALITIES.map(|q| common::psnr(&image, case.format, &EncodeOptions::with_quality(q)).0);
        println!(
            "{:<14?} {:<11} fast {:6.2}  normal {:6.2}  high {:6.2} dB",
            case.format, case.image, psnr[0], psnr[1], psnr[2]
        );
        for (q, (&p, &floor)) in QUALITIES.iter().zip(psnr.iter().zip(&case.floors)) {
            if p < floor {
                failures.push(format!(
                    "{:?} {} {q:?}: {p:.2} dB < {floor:.2}",
                    case.format, case.image
                ));
            }
        }
        // More effort never loses much (the searches are not nested, so allow a little).
        assert!(
            psnr[1] >= psnr[0] - 0.3 && psnr[2] >= psnr[1] - 0.3,
            "{:?} {}: {psnr:?}",
            case.format,
            case.image
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The PSNR of `data` decoded against `image`, as [`common::psnr`] measures it.
fn decoded_psnr(image: &Image, format: Format, data: &[u8]) -> f64 {
    let decoded = decode(data, image.width, image.height, format);
    let source = image.texels_in(format.color_space());
    if format.is_hdr() {
        let abs = |t: &[[f32; 4]]| t.iter().map(|t| t.map(f32::abs)).collect::<Vec<_>>();
        let half = |t: [f32; 4]| {
            t.map(|v| {
                let v = if format == Format::Bc6hUfloat {
                    v.max(0.0)
                } else {
                    v
                };
                marlin_cook_texture::color::f16_to_f32(marlin_cook_texture::color::f32_to_f16(v))
            })
        };
        let source: Vec<[f32; 4]> = source.into_iter().map(half).collect();
        metrics::hdr_psnr(&abs(&source), &abs(&decoded))
    } else {
        metrics::psnr(&source, &decoded, common::channels(format), 1.0)
    }
}

/// Forcing each mode through `EncodeOptions::mode_mask`: the blocks use it and decode close to
/// the source. The floors are loose: a forced mode can be a poor fit for the content.
#[test]
fn every_mode_round_trips() {
    let cases: [(Format, u32, &str, f64); 7] = [
        (Format::Bc1Unorm, 2, "natural", 28.0),
        (Format::Bc3Unorm, 2, "alpha_ramp", 28.0),
        (Format::Bc4Unorm, 2, "single", 35.0),
        (Format::Bc5Unorm, 2, "normals", 30.0),
        (Format::Bc6hUfloat, 14, "hdr", 20.0),
        (Format::Bc6hSfloat, 14, "hdr_signed", 20.0),
        (Format::Bc7Unorm, 8, "natural", 35.0),
    ];
    for (format, modes, name, floor) in cases {
        for m in 0..modes {
            // BC7's alpha modes get alpha content.
            let name = if format == Format::Bc7Unorm && m >= 4 {
                "alpha_ramp"
            } else {
                name
            };
            let image = image(name);
            for quality in [Quality::Fast, Quality::High] {
                let options = EncodeOptions {
                    quality,
                    mode_mask: 1 << m,
                    ..EncodeOptions::default()
                };
                let data = encode(&image, format, &options);
                let blocks: Vec<&[u8]> = data.chunks_exact(format.block_size() as usize).collect();
                let used = blocks
                    .iter()
                    .filter(|b| block_mode(format, b) == Some(m))
                    .count();
                // BC1 and BC4 endpoints can quantize equal, which reads as the other mode.
                let needed = if format.block_size() == 16
                    && !matches!(format, Format::Bc3Unorm | Format::Bc5Unorm)
                {
                    blocks.len()
                } else {
                    blocks.len() * 9 / 10
                };
                assert!(
                    used >= needed,
                    "{format:?} mode {m} {quality:?}: {used}/{} blocks",
                    blocks.len()
                );
                let psnr = decoded_psnr(&image, format, &data);
                println!("{format:?} mode {m} {quality:?} on {name}: {psnr:.2} dB");
                assert!(
                    psnr >= floor,
                    "{format:?} mode {m} {quality:?}: {psnr:.2} dB < {floor}"
                );
            }
        }
    }
}

/// BC1's 1-bit alpha: transparent exactly where the source is below the cutoff.
#[test]
fn bc1_punch_through_alpha() {
    let image = common::cutout(SIZE, SIZE);
    let source = image.to_f32();
    for quality in QUALITIES {
        let options = EncodeOptions {
            quality,
            alpha_cutoff: Some(0.5),
            ..EncodeOptions::default()
        };
        let data = encode(&image, Format::Bc1UnormSrgb, &options);
        let decoded = decode(&data, SIZE, SIZE, Format::Bc1UnormSrgb);
        for (s, d) in source.iter().zip(&decoded) {
            assert_eq!(
                s[3] < 0.5,
                d[3] == 0.0,
                "{quality:?}: alpha {} decoded {}",
                s[3],
                d[3]
            );
        }
    }
}

/// Binary alpha (0 or 1) survives BC3 and BC7 exactly at every quality.
#[test]
fn binary_alpha_is_exact() {
    let image = common::cutout(SIZE, SIZE);
    let source = image.to_f32();
    for format in [Format::Bc3UnormSrgb, Format::Bc7UnormSrgb] {
        for quality in QUALITIES {
            let data = encode(&image, format, &EncodeOptions::with_quality(quality));
            let decoded = decode(&data, SIZE, SIZE, format);
            for (s, d) in source.iter().zip(&decoded) {
                assert_eq!(s[3], d[3], "{format:?} {quality:?}");
            }
        }
    }
}

/// Cooked data must be byte-identical across runs (cook keys rely on it), threads or not.
#[test]
fn encoding_is_deterministic() {
    let image = common::natural(SIZE, SIZE, 7);
    let hdr = common::hdr(SIZE, SIZE, 7);
    for format in Format::ALL {
        let source = if format.is_hdr() { &hdr } else { &image };
        for quality in QUALITIES {
            let options = EncodeOptions::with_quality(quality);
            assert_eq!(
                encode(source, format, &options),
                encode(source, format, &options),
                "{format:?} {quality:?}"
            );
        }
    }
}
