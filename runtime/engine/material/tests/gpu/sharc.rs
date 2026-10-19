//! The world radiance cache (0010 §4.1): after frames of updates from a still camera, what the
//! cache holds where rays hit, against what the surfaces there reflect. A closed box lined with
//! a glowing material, where every surface reflects ρE / (1 − ρ) (light bounces on through the
//! cache); a floor under the sun and the sky, ρ (E/π · cos θ + L); and the lit wall prefab
//! against the CPU reference of its direct light, averaged over each voxel.

use marlin_material::DefaultMaterial;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::raytracing::Ray;
use marlin_render::scene::MaterialDesc;
use marlin_render::{Camera, Environment};
use marlin_test_scenes::reference::{Tracer, light_contribution, pixel_ray};

use crate::support::cook::{cook, cube, cube_source};
use crate::support::{GLOW, HEIGHT, Harness, WIDTH};

/// Rays through every `step`-th pixel of the harness's view, and those pixels.
fn probe_rays(h: &Harness, step: u32) -> (Vec<Ray>, Vec<(u32, u32)>) {
    let mut rays = Vec::new();
    let mut pixels = Vec::new();
    for y in (step / 2..HEIGHT).step_by(step as usize) {
        for x in (step / 2..WIDTH).step_by(step as usize) {
            let (o, d) = pixel_ray(h.camera, WIDTH, HEIGHT, x, y);
            rays.push(Ray {
                origin: o.to_array(),
                t_min: 0.0,
                direction: d.to_array(),
                t_max: 1e4,
            });
            pixels.push((x, y));
        }
    }
    (rays, pixels)
}

/// A cache dense enough for a 96 × 64 view to fill in a few dozen frames: a path a pixel,
/// coarser voxels, deep averages.
fn dense(h: &mut Harness, voxel_angle: f32) {
    h.sharc.stride = 1;
    h.sharc.voxel_angle = voxel_angle;
    h.sharc.max_frames = 1000.0;
    h.sharc.capacity = 1 << 16;
}

fn dark() -> Environment {
    Environment {
        sun_illuminance: 0.0,
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    }
}

#[test]
fn a_glowing_closed_box_reflects_its_geometric_series() {
    let mut h = Harness::new();
    dense(&mut h, 1.0 / 8.0);
    // A short average: the light bounces on through the cache, each frame closing about
    // (1 − ρ²) / `max_frames` of the gap to the fixed point (paths end at the cache from their
    // second hit), so the first frames' too dark sums must fall out of it.
    h.sharc.max_frames = 16.0;
    // A cube turned inside out: its faces look in.
    let (positions, normals, indices) = cube_source();
    let normals: Vec<[f32; 3]> = normals.iter().map(|n| n.map(|c| -c)).collect();
    let indices: Vec<u32> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|t| [t[0], t[2], t[1]])
        .collect();
    let inside = h.upload(&cook(&positions, &normals, &indices));
    let (albedo, emission) = (0.5f32, 1.0f32);
    let glow = h.material(MaterialDesc {
        permutation: GLOW,
        params: [albedo, albedo, albedo, emission, emission, emission]
            .map(f32::to_bits)
            .to_vec(),
    });
    h.add(
        inside,
        glow,
        Transform::from_scale(Vec3::splat(4.0)),
        [1.0; 4],
    );
    h.updates.environment = Some(dark());
    h.camera = (
        Transform {
            translation: Vec3::new(0.3, -0.2, 0.5),
            rotation: Quat::from_euler_yxz(0.4, 0.2, 0.0),
            scale: Vec3::ONE,
        },
        Camera::default(),
    );
    let (rays, _) = probe_rays(&h, 4);
    let read = h.sharc_frames(200, rays);
    // Every surface reflects ρ times what reaches it: ρ (E + ρ (E + ...)) = ρE / (1 − ρ).
    // The albedo as the material buffer and the atlas hold it: 8-bit square root.
    let rho = ((albedo.sqrt() * 255.0).round() / 255.0).powi(2);
    let expected = rho * emission / (1.0 - rho);
    let mut worst = 0.0f32;
    for r in &read {
        let r = r.expect("every ray hits the box");
        assert!(r[3] > 0.0, "a probe in a voxel without samples");
        for c in &r[..3] {
            worst = worst.max((c / expected - 1.0).abs());
        }
    }
    assert!(worst < 0.03, "off by {worst} of {expected}: {read:?}");
}

#[test]
fn a_floor_reflects_the_sun_and_the_sky() {
    let mut h = Harness::new();
    dense(&mut h, 1.0 / 8.0);
    let c = h.upload(&cube());
    let base = [0.6, 0.5, 0.4];
    let material = h.default_material(DefaultMaterial {
        base_color: base,
        roughness: 1.0,
        metallic: 0.0,
    });
    // Its top face at y = 0.
    h.add(
        c,
        material,
        Transform {
            translation: Vec3::new(0.0, -0.1, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(200.0, 0.2, 200.0),
        },
        [1.0; 4],
    );
    let sun = Vec3::new(0.3, -1.0, -0.4);
    let env = Environment {
        light_direction: sun,
        light_color: [1.0, 0.9, 0.8],
        sun_illuminance: core::f32::consts::PI,
        ambient: [0.1, 0.2, 0.4],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    };
    h.updates.environment = Some(env);
    h.camera = (
        Transform {
            translation: Vec3::new(0.0, 3.0, 4.0),
            rotation: Quat::from_rotation_x(-0.6),
            scale: Vec3::ONE,
        },
        Camera::default(),
    );
    let (rays, _) = probe_rays(&h, 4);
    let read = h.sharc_frames(16, rays);
    let cos = (-sun.normalize()).y;
    let expected: [f32; 3] =
        core::array::from_fn(|i| base[i] * (env.light_color[i] * cos + env.ambient[i]));
    let mut hits = 0;
    for r in read.iter().flatten() {
        hits += 1;
        assert!(r[3] > 0.0, "a probe in a voxel without samples");
        for i in 0..3 {
            assert!(
                (r[i] / expected[i] - 1.0).abs() < 0.02,
                "{r:?} against {expected:?}"
            );
        }
    }
    assert!(hits > 100, "only {hits} probes hit the floor");
}

/// The voxel `p` (facing `n`) falls in, as `SharcKey` quantizes it: its corner and size.
fn voxel(p: Vec3, eye: Vec3, angle: f32) -> (Vec3, f32) {
    let d = (p - eye).length().max(0.001);
    let level = ((d * angle).log2().floor() as i32 + 16).clamp(0, 31);
    let size = 2f32.powi(level - 16);
    ((p / size).floor() * size, size)
}

#[test]
fn the_lit_wall_matches_the_reference_over_each_voxel() {
    let scene = marlin_test_scenes::scenes::lit_wall();
    let mut h = Harness::new();
    let angle = 1.0 / 16.0;
    dense(&mut h, angle);
    h.load(&scene);
    let (rays, _) = probe_rays(&h, 6);
    let read = h.sharc_frames(160, rays.clone());
    let tracer = Tracer::new(&scene);
    let albedo = scene.instances[0].albedo;
    let eye = h.camera.0.translation;
    let (mut sum, mut reference_sum) = (0.0f32, 0.0f32);
    let mut checked = 0;
    for (ray, r) in rays.iter().zip(&read) {
        let r = r.expect("every ray hits the wall");
        let hit = tracer
            .trace(Vec3::from(ray.origin), Vec3::from(ray.direction), 1e4)
            .unwrap();
        // Each voxel is a few hundred samples of one resampled light a vertex: within 15%; the
        // whole wall tightly, below.
        // The reference averaged over the voxel's face on the wall (z fixed), 8 × 8 points.
        let (corner, size) = voxel(hit.point, eye, angle);
        let mut reference = [0.0f32; 3];
        for j in 0..8 {
            for i in 0..8 {
                let p = Vec3::new(
                    corner.x + (i as f32 + 0.5) / 8.0 * size,
                    corner.y + (j as f32 + 0.5) / 8.0 * size,
                    hit.point.z,
                );
                for light in &scene.lights {
                    let c = light_contribution(light, p, hit.normal, albedo);
                    for k in 0..3 {
                        reference[k] += c[k] / 64.0;
                    }
                }
            }
        }
        let luminance = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let (got, want) = (luminance([r[0], r[1], r[2]]), luminance(reference));
        assert!(
            (got / want - 1.0).abs() < 0.15,
            "at {:?}: {got} against {want} ({} samples)",
            hit.point,
            r[3]
        );
        sum += got;
        reference_sum += want;
        checked += 1;
    }
    assert!(checked > 100);
    // Unbiased: the whole wall within a few percent.
    assert!(
        (sum / reference_sum - 1.0).abs() < 0.03,
        "{sum} against {reference_sum}"
    );
}
