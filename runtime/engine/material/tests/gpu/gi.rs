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
