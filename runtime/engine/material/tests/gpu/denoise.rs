//! The denoiser (0010 §6). On synthetic signals of known statistics over a flat wall: it is one
//! denoiser for every producer, so what it must get right is the hints (0010 §6.1). Noise that
//! persists over frames, declared so (`Novelty` < 1), denoises as well as fresh noise; the same
//! noise declared fresh, as a classic denoiser assumes, does not. Then on ReSTIR's light over
//! the prefab scenes, against the CPU reference.

use marlin_gpu_test_support::{Image, assert_image};
use marlin_math::Rng;
use marlin_test_scenes::reference::direct_lighting;
use marlin_test_scenes::scenes::{cornell_box, light_hall, lit_wall};

use crate::restir::errors;

use crate::support::{HEIGHT, Harness, WIDTH};

const PIXELS: usize = (WIDTH * HEIGHT) as usize;

/// A noise field over the view: light `2u` (u uniform in [0, 1): mean 1, deviation 0.577),
/// gray, no hit distance.
fn field(rng: &mut Rng) -> Vec<[f32; 4]> {
    (0..PIXELS)
        .map(|_| {
            let v = 2.0 * rng.f32();
            [v, v, v, 0.0]
        })
        .collect()
}

/// Denoises `frames` frames of noise around `truth` whose field changes every `hold` frames,
/// declared with `novelty`; returns the last frame's RMS error against `truth`.
fn run(frames: u32, hold: u32, novelty: f32, truth: impl Fn(u32) -> f32) -> f32 {
    let mut h = Harness::new();
    h.load(&lit_wall());
    let mut rng = Rng::new(17);
    let hints = vec![[novelty, 1.0]; PIXELS];
    let mut noise = field(&mut rng);
    let mut out = Vec::new();
    for frame in 0..frames {
        if frame % hold == 0 {
            noise = field(&mut rng);
        }
        let t = truth(frame);
        let signal: Vec<[f32; 4]> = noise.iter().map(|n| n.map(|v| v * t)).collect();
        out = h.denoise_signal(frame + 1, &signal, &hints);
    }
    let t = truth(frames - 1);
    let sum: f32 = out.iter().map(|p| (p[1] - t) * (p[1] - t)).sum();
    (sum / PIXELS as f32).sqrt() / t
}

#[test]
fn persistent_noise_declared_so_denoises_like_fresh_noise() {
    let frames = 256;
    let fresh = run(frames, 1, 1.0, |_| 1.0);
    let persistent = run(frames, 8, 1.0 / 8.0, |_| 1.0);
    let undeclared = run(frames, 8, 1.0, |_| 1.0);
    eprintln!(
        "RMS error: fresh {fresh:.4}, persistent declared {persistent:.4}, persistent as fresh \
         {undeclared:.4}"
    );
    // 256 frames of noise held 8 frames each are 32 independent fields: averaging them all leaves
    // 0.577 / √32 = 0.102; the blur takes some off, the clamp puts some back (measured: 0.128).
    let limit = 0.577 / 32f32.sqrt();
    assert!(
        persistent < 1.4 * limit,
        "declared persistent noise: {persistent}"
    );
    // Declared fresh, its 32 frames are 4 fields (measured: 0.181).
    assert!(
        undeclared > 1.3 * persistent,
        "as fresh {undeclared}, declared {persistent}"
    );
    // Fresh noise: 32 frames' worth (measured: 0.061).
    assert!(fresh < 0.08, "fresh noise: {fresh}");
}

#[test]
fn a_change_beyond_the_noise_is_followed() {
    // The light triples at frame 200. The fast accumulation follows within a few frames and the
    // slow one is held near it; with persistent noise only as fast as new fields arrive
    // (measured: 0.115 fresh, 16 frames after; 0.211 persistent, 32 frames after).
    let step = |f| if f < 200 { 1.0 } else { 3.0 };
    let fresh = run(216, 1, 1.0, step);
    let persistent = run(232, 8, 1.0 / 8.0, step);
    eprintln!(
        "RMS error after the change: fresh {fresh:.4} (16 frames), persistent {persistent:.4} (32)"
    );
    assert!(fresh < 0.15, "fresh, 16 frames after: {fresh}");
    assert!(
        persistent < 0.25,
        "persistent, 32 frames after: {persistent}"
    );
}

/// ReSTIR's frame `frames` of `scene`, denoised or not, and its mean relative error, the 90th
/// percentile of the per-pixel one and its energy against the reference.
fn restir_after(
    scene: &marlin_test_scenes::scenes::Scene,
    frames: u32,
    denoise: bool,
) -> (Vec<[f32; 3]>, f32, f32, f32) {
    let reference = direct_lighting(scene, WIDTH, HEIGHT, 1);
    let mut h = Harness::new();
    h.load(scene);
    h.denoise = denoise;
    let mut frame = Vec::new();
    for seed in 0..frames {
        frame = h.restir_frame(seed);
    }
    let (mean, p90) = errors(&frame, &reference, 0.0);
    let energy = frame
        .iter()
        .zip(&reference)
        .filter_map(|(p, r)| r.map(|_| p[1]))
        .sum::<f32>()
        / reference.iter().flatten().map(|r| r[1]).sum::<f32>();
    (frame, mean, p90, energy)
}

#[test]
fn denoising_restir_lowers_the_error() {
    // 256 lights over pillars, frame 64 (measured: 0.341 raw, 0.116 denoised; 90th percentile
    // 0.665, 0.256).
    let scene = light_hall(256);
    let (_, raw, raw90, _) = restir_after(&scene, 64, false);
    let (frame, denoised, denoised90, _) = restir_after(&scene, 64, true);
    eprintln!("error {raw:.4} raw, {denoised:.4} denoised; p90 {raw90:.3}, {denoised90:.3}");
    assert!(denoised < 0.5 * raw, "{denoised} denoised, {raw} raw");
    assert!(
        denoised90 < 0.5 * raw90,
        "{denoised90} denoised, {raw90} raw"
    );
    // The lights are colored: each channel's light is the reference's (measured: within 3%).
    let reference = direct_lighting(&scene, WIDTH, HEIGHT, 1);
    for c in 0..3 {
        let sum = |f: &mut dyn Iterator<Item = f32>| f.sum::<f32>();
        let ours = sum(&mut frame
            .iter()
            .zip(&reference)
            .filter_map(|(p, r)| r.map(|_| p[c])));
        let theirs = sum(&mut reference.iter().flatten().map(|r| r[c]));
        assert!(
            (ours / theirs - 1.0).abs() < 0.05,
            "channel {c}: {ours} denoised, {theirs} reference"
        );
    }
}

#[test]
fn denoised_restir_stays_near_the_reference() {
    // Converged (frame 256): what is left is the blur's bias at sharp lights (measured: Cornell
    // box 0.017, energy 0.992; lit wall 0.010, 0.993).
    for scene in [cornell_box(), lit_wall()] {
        let (_, mean, _, energy) = restir_after(&scene, 256, true);
        eprintln!("{}: error {mean:.4}, energy {energy:.4}", scene.name);
        assert!(mean < 0.03, "{}: error {mean}", scene.name);
        assert!(
            (0.98..1.02).contains(&energy),
            "{}: energy {energy}",
            scene.name
        );
    }
}

#[test]
fn the_history_follows_the_camera() {
    // After 64 frames the camera steps 0.3 m: the accumulation is found through the motion
    // vectors, so the next frame is near the moved view's own 64th (measured: 0.223 and 0.166);
    // lost, it would be as noisy as a first frame (0.34).
    let scene = light_hall(256);
    let mut moved = scene.clone();
    moved.camera.0.translation += marlin_math::Vec3::new(0.3, 0.0, 0.0);
    let reference = direct_lighting(&moved, WIDTH, HEIGHT, 1);
    let mut h = Harness::new();
    h.load(&scene);
    h.denoise = true;
    for seed in 0..64 {
        h.restir_frame(seed);
    }
    h.camera = moved.camera;
    let (after, _) = errors(&h.restir_frame(100), &reference, 0.0);
    let (_, steady, _, _) = restir_after(&moved, 64, true);
    eprintln!("after the step {after:.4}, the moved view's 64th frame {steady:.4}");
    assert!(
        after < 1.4 * steady,
        "after the step {after}, steady {steady}"
    );
}

#[test]
fn the_denoised_cornell_box_looks_as_it_did() {
    let (frame, _, _, _) = restir_after(&cornell_box(), 64, true);
    // Reinhard, then sRGB-ish: a picture to look at, not to measure.
    let pixels: Vec<u8> = frame
        .iter()
        .flat_map(|p| {
            let [r, g, b] = p.map(|c| {
                let c = c * 4.0;
                ((c / (1.0 + c)).powf(1.0 / 2.2) * 255.0).round() as u8
            });
            [r, g, b, 255]
        })
        .collect();
    assert_image!("cornell_box_denoised", &Image::new(WIDTH, HEIGHT, pixels));
}

#[test]
fn the_denoiser_costs_little() {
    // At 640 × 360 (measured: 0.25 ms on an RTX 5060, so about 2.2 ms at 1080p); a budget, not a
    // benchmark.
    let mut h = Harness::new();
    h.load(&light_hall(256));
    h.denoise = true;
    let mut times = Vec::new();
    for seed in 0..24 {
        h.restir_frame_at(seed, 640, 360);
        if seed >= 8 {
            let t: f64 = h
                .pass_times()
                .iter()
                .filter(|(n, _)| n.starts_with("Denoise"))
                .map(|(_, t)| t)
                .sum();
            times.push(t);
        }
    }
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    eprintln!("the denoiser at 640 × 360: {median:.3} ms");
    assert!(median > 0.0, "no pass times");
    assert!(median < 0.5, "{median:.3} ms");
}
