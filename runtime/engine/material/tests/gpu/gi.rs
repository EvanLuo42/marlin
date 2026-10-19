//! ReSTIR with GI (R4c): the reservoirs owning indirect light too (0010 §2.9), a bounce from each
//! pixel's surface a candidate beside the lights, the world cache beyond. The Cornell box,
//! averaged over frames, against the CPU reference of its global illumination
//! (`marlin_test_scenes::reference::global_illumination`).

use marlin_test_scenes::reference::{direct_lighting, global_illumination};
use marlin_test_scenes::scenes::cornell_box;

use crate::restir::{average, errors};
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::MaterialDesc;
use marlin_render::{Camera, Environment};

use crate::support::{GLOW, HEIGHT, Harness, WIDTH};

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

/// A mirror floor (white metal, roughness 0) with emissive blocks of four colors standing on it,
/// in the dark: the floor's light is their reflection alone, exact every frame (the blocks
/// glow and reflect nothing; no light, no sky), so what a pan smears is all the error there is.
fn mirror_and_glow(h: &mut Harness) {
    let cube = h.upload(&crate::support::cook::cube());
    let mirror = h.default_material(marlin_material::DefaultMaterial {
        base_color: [0.9; 3],
        roughness: 0.0,
        metallic: 1.0,
    });
    h.add(
        cube,
        mirror,
        Transform {
            translation: Vec3::new(0.0, -0.05, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(8.0, 0.1, 8.0),
        },
        [1.0; 4],
    );
    for (k, color) in [
        [4.0, 0.5, 0.5],
        [0.5, 4.0, 0.5],
        [0.5, 0.5, 4.0],
        [4.0, 4.0, 0.5],
    ]
    .into_iter()
    .enumerate()
    {
        let glow = h.material(MaterialDesc {
            permutation: GLOW,
            params: [0.0, 0.0, 0.0, color[0], color[1], color[2]]
                .map(f32::to_bits)
                .to_vec(),
        });
        h.add(
            cube,
            glow,
            Transform {
                translation: Vec3::new(-1.5 + k as f32, 0.4, -1.5),
                rotation: Quat::IDENTITY,
                scale: Vec3::new(0.5, 0.8, 0.5),
            },
            [1.0; 4],
        );
    }
    h.updates.environment = Some(Environment {
        sun_illuminance: 0.0,
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    });
}

/// The camera `x` to the side, a little above the floor looking at the blocks' reflections.
fn mirror_camera(x: f32) -> (Transform, Camera) {
    (
        Transform {
            translation: Vec3::new(x, 1.0, 2.5),
            rotation: Quat::from_rotation_x(-0.35),
            scale: Vec3::ONE,
        },
        Camera::default(),
    )
}

/// The mean error over the floor's reflections (the bottom half of the view) of a denoised
/// frame after a sideways pan of `pan` m in 20 frames, against a still frame from where it ends;
/// the denoiser's virtual motion on or off.
fn panned_mirror_error(virtual_motion: bool, pan: f32) -> f32 {
    let mut still = gi_harness();
    still.denoise = true;
    mirror_and_glow(&mut still);
    still.camera = mirror_camera(pan);
    let mut reference = Vec::new();
    for seed in 0..32 {
        reference = still.restir_frame(seed);
    }
    let mut h = gi_harness();
    h.denoise = true;
    h.denoiser.virtual_motion = virtual_motion;
    mirror_and_glow(&mut h);
    h.camera = mirror_camera(-pan);
    for seed in 0..32 {
        h.restir_frame(seed);
    }
    let mut picture = Vec::new();
    for k in 0..20 {
        h.camera = mirror_camera(-pan + 2.0 * pan * (k + 1) as f32 / 20.0);
        picture = h.restir_frame(100 + k);
    }
    let (mut error, mut total) = (0.0f32, 0.0f32);
    for i in (HEIGHT / 2 * WIDTH) as usize..(HEIGHT * WIDTH) as usize {
        for c in 0..3 {
            error += (picture[i][c] - reference[i][c]).abs();
            total += reference[i][c];
        }
    }
    error / total
}

#[test]
fn a_panned_mirror_keeps_its_reflection_in_place() {
    // A reflection moves as its virtual point does (R4d3): denoised along the surface's motion,
    // the floor's reflections smear behind a pan.
    let still = panned_mirror_error(true, 0.0);
    let (virtual_motion, surface_motion) = (
        panned_mirror_error(true, 0.5),
        panned_mirror_error(false, 0.5),
    );
    // The reflections are exact every frame: following them, the history is too (the hit distance
    // test keeps a reflection's edges from taking history from what they uncover).
    assert!(still < 0.02, "the still mirror: {still}");
    assert!(virtual_motion < 0.02, "after the pan: {virtual_motion}");
    // And the test sees a smear where there is one.
    assert!(
        surface_motion > 0.2,
        "along the surface's motion: {surface_motion}"
    );
}
