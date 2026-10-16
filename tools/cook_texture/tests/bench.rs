//! Encoder throughput and quality. `cargo test --release -p marlin-cook-texture --test bench --
//! --ignored --nocapture`.

mod common;

use std::time::Instant;

use marlin_cook_texture::{
    EncodeOptions, Format, Image, MipOptions, Quality, encode, generate_mips,
};

const SIZE: u32 = 1024;

fn row(name: &str, format: Format, image: &Image) {
    for quality in [Quality::Fast, Quality::Normal, Quality::High] {
        let options = EncodeOptions::with_quality(quality);
        // Warm up the thread pool and tables, then time one full encode.
        encode(&common::gradient(64, 64), format, &options);
        let start = Instant::now();
        let data = encode(image, format, &options);
        let seconds = start.elapsed().as_secs_f64();
        let mpix = (image.width * image.height) as f64 / 1e6 / seconds;
        let (psnr, _) = common::psnr(image, format, &options);
        println!(
            "{format:<14?} {name:<10} {quality:<6?} {mpix:>8.2} Mpix/s  {psnr:>6.2} dB  ({} bytes)",
            data.len()
        );
    }
}

#[test]
#[ignore]
fn throughput() {
    let natural = common::natural(SIZE, SIZE, 1);
    let alpha = common::alpha_ramp(SIZE, SIZE);
    let normals = common::normal_map(SIZE, SIZE);
    let single = common::single_channel(SIZE, SIZE, 3);
    let hdr = common::hdr(SIZE, SIZE, 2);
    println!(
        "{SIZE}x{SIZE}, {} threads",
        std::thread::available_parallelism().unwrap()
    );
    row("natural", Format::Bc1UnormSrgb, &natural);
    row("alpha", Format::Bc3UnormSrgb, &alpha);
    row("single", Format::Bc4Unorm, &single);
    row("normals", Format::Bc5Unorm, &normals);
    row("hdr", Format::Bc6hUfloat, &hdr);
    row("natural", Format::Bc7UnormSrgb, &natural);
    row("alpha", Format::Bc7UnormSrgb, &alpha);

    let start = Instant::now();
    let chain = generate_mips(&natural, &MipOptions::default());
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "mips (Kaiser, sRGB) {:.1} Mpix/s of level 0, {} levels",
        (SIZE * SIZE) as f64 / 1e6 / seconds,
        chain.len()
    );
}
