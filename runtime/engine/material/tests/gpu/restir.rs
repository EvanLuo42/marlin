//! ReSTIR DI (R3c) on the prefab test scenes, averaged over frames, against the CPU reference of
//! their direct lighting (`marlin_test_scenes::reference`): point lights on a wall, and the
//! Cornell box with its sphere light's soft shadows.

use marlin_gpu_test_support::{Image, assert_image};
use marlin_test_scenes::reference::direct_lighting;
use marlin_test_scenes::scenes::{cornell_box, light_hall, lit_wall};

use crate::support::{HEIGHT, Harness, WIDTH};

/// The mean over `frames` frames of ReSTIR's picture.
fn average(h: &mut Harness, frames: u32) -> Vec<[f32; 3]> {
    let mut sum = vec![[0.0f32; 3]; (WIDTH * HEIGHT) as usize];
    for seed in 1..=frames {
        for (s, p) in sum.iter_mut().zip(h.restir_frame(seed)) {
            for i in 0..3 {
                s[i] += p[i];
            }
        }
    }
    sum.iter().map(|s| s.map(|v| v / frames as f32)).collect()
}

/// The mean relative error of `picture` against `reference` (green, where the reference sees a
/// surface), and the 90th percentile of the per-pixel relative error over pixels lit above
/// `floor` of the brightest.
fn errors(picture: &[[f32; 3]], reference: &[Option<[f32; 3]>], floor: f32) -> (f32, f32) {
    let brightest = reference
        .iter()
        .flatten()
        .map(|r| r[1])
        .fold(0.0f32, f32::max);
    let (mut error, mut total, mut each) = (0.0f32, 0.0f32, Vec::new());
    for (p, r) in picture.iter().zip(reference) {
        let Some(r) = r else { continue };
        error += (p[1] - r[1]).abs();
        total += r[1];
        if r[1] > floor * brightest {
            each.push((p[1] - r[1]).abs() / r[1]);
        }
    }
    each.sort_by(f32::total_cmp);
    (error / total, each[each.len() * 9 / 10])
}

#[test]
fn point_lights_on_a_wall_converge_to_the_reference() {
    let scene = lit_wall();
    let mut h = Harness::new();
    h.load(&scene);
    let picture = average(&mut h, 64);
    let reference = direct_lighting(&scene, WIDTH, HEIGHT, 1);
    let (mean, p90) = errors(&picture, &reference, 0.0);
    assert!(mean < 0.03, "mean relative error {mean}");
    assert!(p90 < 0.1, "90th percentile of the per-pixel error {p90}");
}

#[test]
fn the_cornell_box_converges_to_the_reference() {
    let scene = cornell_box();
    let mut h = Harness::new();
    h.load(&scene);
    let picture = average(&mut h, 128);
    let reference = direct_lighting(&scene, WIDTH, HEIGHT, 8);
    // The blocks' shadows soften with the light's sphere, which the shadow rays sample one point
    // a frame: noisier than the wall, still close on average.
    let (mean, p90) = errors(&picture, &reference, 0.02);
    assert!(mean < 0.05, "mean relative error {mean}");
    assert!(p90 < 0.2, "90th percentile of the per-pixel error {p90}");
}

#[test]
fn the_cornell_box_looks_as_it_did() {
    let scene = cornell_box();
    let mut h = Harness::new();
    h.load(&scene);
    let picture = average(&mut h, 64);
    // Reinhard, then sRGB-ish: a picture to look at, not to measure.
    let pixels: Vec<u8> = picture
        .iter()
        .flat_map(|p| {
            let [r, g, b] = p.map(|c| {
                let c = c * 4.0;
                ((c / (1.0 + c)).powf(1.0 / 2.2) * 255.0).round() as u8
            });
            [r, g, b, 255]
        })
        .collect();
    assert_image!("cornell_box_restir", &Image::new(WIDTH, HEIGHT, pixels));
}

/// ReSTIR's GPU time a frame (its passes', in milliseconds) on the light hall with `lights`
/// lights at 640 × 360: the median of 16 frames after 8 to warm up.
fn restir_time(lights: usize) -> f64 {
    let mut h = Harness::new();
    h.load(&light_hall(lights));
    let mut times = Vec::new();
    for seed in 0..24 {
        h.restir_frame_at(seed, 640, 360);
        if seed >= 8 {
            let t: f64 = h
                .pass_times()
                .iter()
                .filter(|(n, _)| n.starts_with("Restir") || *n == "PresampleLights")
                .map(|(_, t)| t)
                .sum();
            times.push(t);
        }
    }
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

#[test]
fn the_cost_does_not_grow_with_the_lights() {
    // 0010's R3 acceptance: a thousand lights, frame time independent of their number.
    let few = restir_time(10);
    let many = restir_time(1000);
    eprintln!("ReSTIR DI at 640 × 360: 10 lights {few:.3} ms, 1000 lights {many:.3} ms");
    assert!(few > 0.0, "no pass times");
    assert!(
        many < few * 1.5 + 0.05,
        "10 lights {few:.3} ms, 1000 lights {many:.3} ms"
    );
}
