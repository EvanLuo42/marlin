//! ReSTIR with GI (R4c): the reservoirs owning indirect light too (0010 §2.9), a bounce from each
//! pixel's surface a candidate beside the lights, the world cache beyond. The Cornell box,
//! averaged over frames, against the CPU reference of its global illumination
//! (`marlin_test_scenes::reference::global_illumination`).

use marlin_test_scenes::reference::{direct_lighting, global_illumination};
use marlin_test_scenes::scenes::cornell_box;

use crate::restir::{average, errors};
use crate::support::{HEIGHT, Harness, WIDTH};

/// A harness drawing GI, its world cache dense enough for a 96 × 64 view: a path a pixel, voxels
/// about 6 pixels across (at 1080p a sixteenth of the pixels and the default voxels give each
/// voxel as many paths).
fn gi_harness() -> Harness {
    let mut h = Harness::new();
    h.restir.gi = true;
    h.sharc.stride = 1;
    h.sharc.voxel_angle = 1.0 / 16.0;
    h
}

#[test]
fn the_cornell_box_with_gi_converges_to_the_reference() {
    let scene = cornell_box();
    let mut h = gi_harness();
    h.load(&scene);
    // The cache first: light bounces on through it, filling in over frames.
    average(&mut h, 128);
    let picture = average(&mut h, 128);
    let reference = global_illumination(&scene, WIDTH, HEIGHT, 4, 256);
    let direct = direct_lighting(&scene, WIDTH, HEIGHT, 4);
    // Each channel summed over the pixels the reference has: the picture, the reference, and
    // the direct light alone (what R3's DI drew).
    let total = |of: &dyn Fn(usize) -> f32| -> f32 {
        (0..reference.len())
            .filter(|&i| reference[i].is_some())
            .map(of)
            .sum()
    };
    for c in 0..3 {
        let gpu = total(&|i| picture[i][c]);
        let cpu = total(&|i| reference[i].unwrap()[c]);
        let alone = total(&|i| direct[i].map_or(0.0, |d| d[c]));
        // The indirect light, about two fifths of the box's: within 15% (the cache's voxels and
        // its lag darken the deeper bounces a little), and the whole within 6%.
        let indirect = (gpu - alone) / (cpu - alone);
        assert!(
            (0.85..1.1).contains(&indirect),
            "channel {c}: indirect {indirect} of the reference's ({gpu} against {cpu}, direct {alone})"
        );
        assert!(
            (gpu / cpu - 1.0).abs() < 0.06,
            "channel {c}: {gpu} against {cpu}"
        );
    }
    // Per pixel, both noisy (the reference too: 256 paths).
    let (mean, p90) = errors(&picture, &reference, 0.02);
    assert!(mean < 0.15, "mean relative error {mean}");
    assert!(p90 < 0.35, "90th percentile of the per-pixel error {p90}");
}

/// The Cornell box with its floor metal of `roughness` (below τ: the specular tracing's).
fn reflective_floor(roughness: f32) -> marlin_test_scenes::Scene {
    let mut scene = cornell_box();
    scene.instances[0].albedo = [0.9; 3];
    scene.instances[0].metallic = 1.0;
    scene.instances[0].roughness = roughness;
    scene
}

/// The reflections a floor below τ traces (R4d2) converge to the reference's.
fn reflections_converge(roughness: f32) {
    let scene = reflective_floor(roughness);
    let mut h = gi_harness();
    h.load(&scene);
    average(&mut h, 128);
    let picture = average(&mut h, 128);
    let reference = global_illumination(&scene, WIDTH, HEIGHT, 4, 256);
    // The floor's pixels: the bottom fifth of the view.
    let floor: Vec<usize> = (0..reference.len())
        .filter(|&i| reference[i].is_some() && i as u32 / WIDTH >= HEIGHT * 4 / 5)
        .collect();
    assert!(floor.len() > 20);
    // What the floor reflects includes the walls' indirect light, which the world cache keeps a
    // little dark (blue most, as in the GI test above): within 10%.
    let total = |of: &dyn Fn(usize) -> [f32; 3]| -> [f32; 3] {
        floor.iter().fold([0.0; 3], |s, &i| {
            let p = of(i);
            [s[0] + p[0], s[1] + p[1], s[2] + p[2]]
        })
    };
    let gpu = total(&|i| picture[i]);
    let cpu = total(&|i| reference[i].unwrap());
    for (c, (gpu, cpu)) in gpu.into_iter().zip(cpu).enumerate() {
        assert!(
            (gpu / cpu - 1.0).abs() < 0.1,
            "roughness {roughness}, channel {c}: the floor {gpu} against {cpu}"
        );
    }
    let (mean, _) = errors(&picture, &reference, 0.02);
    assert!(mean < 0.15, "mean relative error {mean}");
}

#[test]
fn a_mirror_floor_reflects_the_box() {
    reflections_converge(0.0);
}

#[test]
fn a_glossy_floor_reflects_the_box() {
    reflections_converge(0.1);
}

#[test]
fn the_split_at_tau_neither_doubles_nor_drops_light() {
    // Just below τ the floor's reflection is traced, just above it resampled (0010 §2.7): the
    // floor about as bright either way (0010 §13).
    let floor = |roughness: f32| -> [f32; 3] {
        let mut h = gi_harness();
        h.load(&reflective_floor(roughness));
        average(&mut h, 128);
        let picture = average(&mut h, 128);
        let rows = (HEIGHT * 4 / 5)..HEIGHT;
        let mut sum = [0.0f32; 3];
        for y in rows {
            for x in 0..WIDTH {
                let p = picture[(y * WIDTH + x) as usize];
                for c in 0..3 {
                    sum[c] += p[c];
                }
            }
        }
        sum
    };
    let (below, above) = (floor(0.19), floor(0.21));
    for c in 0..3 {
        assert!(
            (above[c] / below[c] - 1.0).abs() < 0.05,
            "channel {c}: {} below τ, {} above",
            below[c],
            above[c]
        );
    }
}
