//! Simplified materials (0011 §9.1–9.2): what a ray hit reads (`SimpleOfHit`, the baked atlas
//! times the instance's factor) holds against what the material pass wrote for the same surface
//! (the proxy lobe), through primary rays at pixel centers; and stays so as instances change.

use marlin_material::DefaultMaterial;
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::raytracing::Ray;
use marlin_render::scene::{InstanceId, InstanceUpdate};

use crate::support::cook::cube;
use crate::support::{Frame, HEIGHT, Harness, Proxy, SimpleHit, WIDTH, proxy};

/// A tinted dielectric cube and a metallic one, under the default material.
fn scene(h: &mut Harness) -> [InstanceId; 2] {
    let c = h.upload(&cube());
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
    let at = |x, turn| Transform {
        translation: Vec3::new(x, 0.0, 0.0),
        rotation: Quat::from_euler_yxz(turn, 0.3, 0.0),
        scale: Vec3::splat(1.2),
    };
    [
        h.add(c, dielectric, at(-1.0, 0.5), [0.8, 0.2, 0.2, 1.0]),
        h.add(c, metal, at(1.0, -0.5), [1.0; 4]),
    ]
}

/// Primary rays through every pixel's center, row-major, from the harness's camera.
fn primary_rays(h: &Harness) -> Vec<Ray> {
    let (transform, camera) = h.camera;
    let tan = (camera.fov_y * 0.5).tan();
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let origin = transform.translation;
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| {
            let ndc_x = (2.0 * (x as f32 + 0.5) / WIDTH as f32 - 1.0) * tan * aspect;
            let ndc_y = (1.0 - 2.0 * (y as f32 + 0.5) / HEIGHT as f32) * tan;
            let d = (transform.rotation * Vec3::new(ndc_x, ndc_y, -1.0)).normalize();
            Ray {
                origin: [origin.x, origin.y, origin.z],
                t_min: 0.0,
                direction: [d.x, d.y, d.z],
                t_max: 1e4,
            }
        })
        .collect()
}

fn close(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tolerance)
}

/// Checks every pixel inside a surface (its neighbors the same proxy lobe, so not an edge a ray
/// and the rasterizer may see differently): its hit's simplified material is its proxy lobe.
/// Returns the pixels checked.
fn assert_matches(f: &Frame, hits: &[Option<SimpleHit>]) -> usize {
    // 8-bit square-root encodings on both sides.
    let tolerance = 0.015;
    let p = |x: u32, y: u32| proxy(f.material[(y * WIDTH + x) as usize]);
    let same = |a: Proxy, b: Proxy| a.albedo == b.albedo && a.f0 == b.f0;
    let mut checked = 0;
    for y in 1..HEIGHT - 1 {
        for x in 1..WIDTH - 1 {
            let i = (y * WIDTH + x) as usize;
            if f.visbuffer[i] == 0 {
                continue;
            }
            let here = p(x, y);
            if ![p(x - 1, y), p(x + 1, y), p(x, y - 1), p(x, y + 1)]
                .into_iter()
                .all(|n| same(here, n))
            {
                continue;
            }
            let hit = hits[i]
                .unwrap_or_else(|| panic!("pixel ({x}, {y}) is covered, its ray hit nothing"));
            assert!(
                close(hit.albedo, here.albedo, tolerance)
                    && close(hit.f0, here.f0, tolerance)
                    && (hit.roughness - here.roughness).abs() <= tolerance,
                "pixel ({x}, {y}): the hit's simplified material {hit:?}, the proxy lobe {here:?}"
            );
            assert_eq!(hit.opacity, 1.0);
            assert_eq!(hit.emission, [0.0; 3]);
            checked += 1;
        }
    }
    checked
}

#[test]
fn hits_read_the_proxy_lobe_of_the_surface() {
    let mut h = Harness::new();
    scene(&mut h);
    let f = h.frame(None);
    let hits = h.probe_simple(primary_rays(&h));
    let checked = assert_matches(&f, &hits);
    assert!(checked > 500, "only {checked} pixels inside surfaces");
    // Both cubes' rows, both of their materials.
    let mut rows: Vec<u32> = hits.iter().flatten().map(|h| h.row).collect();
    rows.sort_unstable();
    rows.dedup();
    assert_eq!(rows.len(), 2);
}

#[test]
fn a_changed_instance_color_rebakes_its_factor() {
    let mut h = Harness::new();
    let [tinted, _] = scene(&mut h);
    h.frame(None);
    h.probe_simple(primary_rays(&h));
    // Only the change rebakes now (the atlas and factors are there).
    h.updates
        .instances
        .push(tinted, InstanceUpdate::Color([0.1, 0.7, 0.3, 1.0]));
    let hits = h.probe_simple(primary_rays(&h));
    let f = h.frame(None);
    assert!(assert_matches(&f, &hits) > 500);
}
