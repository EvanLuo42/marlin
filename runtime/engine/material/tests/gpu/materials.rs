//! Classify, bin and resolve: every covered pixel resolved once by its material's permutation,
//! the proxy lobe packed as the materials say, velocities from the instances' and the camera's
//! motion, and the debug views against their references.

use marlin_gpu_test_support::assert_image;
use marlin_material::{DefaultMaterial, MaterialDebug};
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{InstanceUpdate, MaterialDesc};

use crate::support::cook::{cube, sphere};
use crate::support::{DEFAULT, Frame, HEIGHT, Harness, STRIPES, WIDTH, proxy};

const RED: [f32; 4] = [0.8, 0.2, 0.2, 1.0];

fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tolerance)
}

/// Two default-material cubes (one dielectric and tinted, one metallic) and a striped sphere.
fn scene(h: &mut Harness) {
    let (c, s) = (h.upload(&cube()), h.upload(&sphere(16, 32)));
    let dielectric = h.default_material(DefaultMaterial {
        base_color: [1.0; 3],
        roughness: 0.3,
        metallic: 0.0,
    });
    let metal = h.default_material(DefaultMaterial {
        base_color: [0.9, 0.6, 0.2],
        roughness: 0.6,
        metallic: 1.0,
    });
    let stripes = h.material(MaterialDesc {
        permutation: STRIPES,
        params: [1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.25]
            .iter()
            .map(|f: &f32| f.to_bits())
            .collect(),
    });
    let at = |x, y, z, turn| Transform {
        translation: Vec3::new(x, y, z),
        rotation: Quat::from_euler_yxz(turn, 0.3, 0.0),
        scale: Vec3::ONE,
    };
    h.add(c, dielectric, at(-1.3, 0.4, 0.0, 0.5), RED);
    h.add(c, metal, at(1.3, 0.4, 0.0, -0.5), [1.0; 4]);
    h.add(
        s,
        stripes,
        Transform {
            translation: Vec3::new(0.0, -0.5, 0.6),
            rotation: Quat::IDENTITY,
            scale: Vec3::splat(1.4),
        },
        [1.0; 4],
    );
}

fn covered(f: &Frame) -> usize {
    f.visbuffer.iter().filter(|&&v| v != 0).count()
}

#[test]
fn every_covered_pixel_is_resolved_once_by_its_material() {
    let mut h = Harness::new();
    scene(&mut h);
    let f = h.frame(None);
    let n = covered(&f);
    assert!(n > 0);
    assert!(f.counts[DEFAULT as usize] > 0 && f.counts[STRIPES as usize] > 0);
    assert_eq!(
        f.counts.iter().sum::<u32>() as usize,
        n,
        "every covered pixel in one bin"
    );

    // An 8-bit square-root-encoded color is within this of the value it encodes.
    let tolerance = 0.01;
    let (mut red, mut metal, mut light, mut dark) = (0, 0, 0, 0);
    for (p, &d) in f.material.iter().enumerate() {
        if f.visbuffer[p] == 0 {
            assert_eq!(d, [0; 4], "pixel {p}: no surface, no closures");
            continue;
        }
        let m = proxy(d);
        assert_eq!(m.count, 1, "pixel {p}");
        assert!(m.fast, "pixel {p}: one plain Slab is the fast path");
        let l = m.normal.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!((l - 1.0).abs() < 1e-3);
        if close(m.albedo, [0.8, 0.2, 0.2], tolerance) {
            assert!(close(m.f0, [0.04; 3], tolerance) && (m.roughness - 0.3).abs() < 0.005);
            red += 1;
        } else if close(m.albedo, [0.0; 3], tolerance) && close(m.f0, [0.9, 0.6, 0.2], tolerance) {
            assert!((m.roughness - 0.6).abs() < 0.005);
            metal += 1;
        } else if close(m.albedo, [1.0; 3], tolerance) {
            assert!((m.roughness - 0.8).abs() < 0.005);
            light += 1;
        } else if close(m.albedo, [0.0; 3], tolerance) && close(m.f0, [0.04; 3], tolerance) {
            assert!((m.roughness - 0.8).abs() < 0.005);
            dark += 1;
        } else {
            panic!("pixel {p}: {m:?} is no material's");
        }
    }
    assert!(
        red > 0 && metal > 0 && light > 0 && dark > 0,
        "{red} {metal} {light} {dark}"
    );
    assert_eq!(light + dark, f.counts[STRIPES as usize] as usize);
}

#[test]
fn normals_are_the_surfaces() {
    let mut h = Harness::new();
    let c = h.upload(&cube());
    let m = h.default_material(DefaultMaterial::default());
    h.add(c, m, Transform::IDENTITY, [1.0; 4]);
    let f = h.frame(None);
    // The front face, straight on.
    let center = (HEIGHT / 2 * WIDTH + WIDTH / 2) as usize;
    let n = proxy(f.material[center]).normal;
    assert!(n[2] > 0.9999, "{n:?}");
}

#[test]
fn velocities_follow_the_instances_and_the_camera() {
    let mut h = Harness::new();
    let c = h.upload(&cube());
    let m = h.default_material(DefaultMaterial::default());
    let cube = h.add(c, m, Transform::IDENTITY, [1.0; 4]);
    // Nothing moved yet.
    let f = h.frame(None);
    assert!(
        f.velocity
            .iter()
            .all(|v| v[0].abs() < 1e-4 && v[1].abs() < 1e-4)
    );

    // The cube moves right by 0.1: its front face (3.5 from the eye) moves 0.1·P00/3.5 in NDC,
    // half that in UV.
    let dx = 0.1;
    h.updates.instances.push(
        cube,
        InstanceUpdate::Transform(Transform::from_translation(Vec3::new(dx, 0.0, 0.0))),
    );
    let f = h.frame(None);
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let p00 = 1.0 / (aspect * (core::f32::consts::FRAC_PI_3 * 0.5).tan());
    let expected = dx * p00 / 3.5 * 0.5;
    let center = (HEIGHT / 2 * WIDTH + WIDTH / 2) as usize;
    let v = f.velocity[center];
    assert!(
        (v[0] - expected).abs() < 2e-3 && v[1].abs() < 1e-3,
        "{v:?}, {expected}"
    );
    // The sky stays.
    assert!(f.velocity[0][0].abs() < 1e-4 && f.velocity[0][1].abs() < 1e-4);

    // The camera turns left: the sky moves right by tan(θ)·P00 in NDC near the center.
    let turn = 0.05f32;
    h.camera.0.rotation = Quat::from_rotation_y(turn);
    let f = h.frame(None);
    let expected = turn.tan() * p00 * 0.5;
    let sky = (2 * WIDTH + WIDTH / 2) as usize;
    assert_eq!(f.visbuffer[sky], 0);
    let v = f.velocity[sky];
    assert!(
        (v[0] - expected).abs() < 0.05 * expected,
        "{v:?}, {expected}"
    );
}

#[test]
fn debug_views_of_the_material_buffer() {
    let mut h = Harness::new();
    scene(&mut h);
    for (mode, name) in [
        (MaterialDebug::Albedo, "material_albedo"),
        (MaterialDebug::Normal, "material_normal"),
        (MaterialDebug::Specular, "material_specular"),
    ] {
        let f = h.frame(Some(mode));
        assert_image!(name, f.debug.as_ref().unwrap());
    }
}
