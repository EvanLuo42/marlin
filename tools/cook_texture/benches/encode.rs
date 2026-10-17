//! Encoder throughput and quality on 1024x1024 images. `cargo xtask test --tier bench -p
//! marlin-cook-texture`, or `cargo bench -p marlin-cook-texture --bench encode -- [filter]`.

#[path = "../tests/support/mod.rs"]
mod support;

use marlin_bench::{Bench, Duration};
use marlin_cook_texture::{
    EncodeOptions, Format, Image, MipOptions, Quality, encode, generate_mips,
};

const SIZE: u32 = 1024;
const PIXELS: u64 = SIZE as u64 * SIZE as u64;

/// A full encode takes up to seconds; a few samples are enough.
fn slow(name: String) -> Bench {
    Bench::new(name)
        .throughput_items(PIXELS, "pix")
        .samples(5)
        .warmup(Duration::ZERO)
}

fn row(name: &str, format: Format, image: &Image) {
    for quality in [Quality::Fast, Quality::Normal, Quality::High] {
        let bench = slow(format!("{format:?}/{name}/{quality:?}"));
        if !bench.selected() {
            continue;
        }
        let options = EncodeOptions::with_quality(quality);
        // Also warms up the thread pool and tables.
        let bytes = encode(image, format, &options).len();
        let (psnr, _) = support::psnr(image, format, &options);
        bench
            .metric("psnr_db", psnr)
            .metric("bytes", bytes as f64)
            .run(|| encode(image, format, &options));
    }
}

fn throughput() {
    let natural = support::natural(SIZE, SIZE, 1);
    let alpha = support::alpha_ramp(SIZE, SIZE);
    let normals = support::normal_map(SIZE, SIZE);
    let single = support::single_channel(SIZE, SIZE, 3);
    let hdr = support::hdr(SIZE, SIZE, 2);
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

    // Throughput counts level 0's pixels.
    let levels = generate_mips(&natural, &MipOptions::default()).len();
    slow("mips/kaiser_srgb".into())
        .metric("levels", levels as f64)
        .run(|| generate_mips(&natural, &MipOptions::default()));
}

marlin_bench::main!(throughput);
