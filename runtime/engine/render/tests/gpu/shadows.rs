//! The sun's virtual shadow maps (R2f): the shadow mask against shadows the CPU finds by casting
//! a ray from each pixel's surface toward the sun through the scene's boxes, and the pages the
//! frame marks.

use marlin_math::{Mat4, Quat, Transform, Vec3, Vec4};
use marlin_render::scene::{InstanceId, InstanceUpdate};
use marlin_render::{Camera, Environment};

use crate::support::{Harness, SHADOW_SETTINGS, SIZE, ShadowFrame};
use crate::visibility::settle;

/// An axis-aligned box: center and half size.
#[derive(Clone, Copy)]
struct Box3 {
    center: Vec3,
    half: Vec3,
}

/// Whether the ray from `origin` along `dir` meets `b` beyond a hair.
fn hits(b: &Box3, origin: Vec3, dir: Vec3) -> bool {
    let (mut near, mut far) = (1e-3f32, f32::INFINITY);
    for i in 0..3 {
        let (o, d, c, h) = (origin[i], dir[i], b.center[i], b.half[i]);
        if d.abs() < 1e-9 {
            if (o - c).abs() > h {
                return false;
            }
            continue;
        }
        let (t0, t1) = ((c - h - o) / d, (c + h - o) / d);
        near = near.max(t0.min(t1));
        far = far.min(t0.max(t1));
    }
    near <= far
}

/// A camera above and in front of the origin, looking down at it.
fn camera() -> (Transform, Camera) {
    (
        Transform {
            translation: Vec3::new(0.0, 2.6, 3.2),
            rotation: Quat::from_rotation_x(-0.65),
            scale: Vec3::ONE,
        },
        Camera::default(),
    )
}

/// A ground slab and a box floating above it: the boxes, for the CPU, and their instances.
fn scene(h: &mut Harness) -> (Vec<Box3>, Vec<InstanceId>) {
    h.camera = Some(camera());
    let boxes = [
        Box3 {
            center: Vec3::new(0.0, -0.6, 0.0),
            half: Vec3::new(4.0, 0.1, 4.0),
        },
        Box3 {
            center: Vec3::new(0.0, 0.6, 0.0),
            half: Vec3::splat(0.5),
        },
    ];
    let ids = boxes
        .iter()
        .map(|b| {
            h.add(
                h.cube,
                Transform {
                    translation: b.center,
                    rotation: Quat::IDENTITY,
                    scale: b.half * 2.0,
                },
                [1.0; 4],
            )
        })
        .collect();
    (boxes.to_vec(), ids)
}

/// What the CPU expects of each pixel: `None` for the sky, else whether the sun is hidden.
fn expected(f: &ShadowFrame, boxes: &[Box3], light: Vec3) -> Vec<Option<bool>> {
    let (eye, cam) = camera();
    let vp =
        Mat4::perspective_infinite_rh(cam.fov_y, 1.0, cam.near) * eye.to_matrix().inverse_affine();
    let inverse = vp.inverse();
    let toward = -light.normalize();
    (0..(SIZE * SIZE) as usize)
        .map(|p| {
            let depth = f.depth[p];
            if depth <= 0.0 {
                return None;
            }
            let (x, y) = ((p as u32 % SIZE) as f32, (p as u32 / SIZE) as f32);
            let ndc = Vec4::new(
                (x + 0.5) / SIZE as f32 * 2.0 - 1.0,
                1.0 - (y + 0.5) / SIZE as f32 * 2.0,
                depth,
                1.0,
            );
            let w = inverse * ndc;
            let world = w.truncate() / w.w;
            Some(boxes.iter().any(|b| hits(b, world, toward)))
        })
        .collect()
}

/// Pixels the GPU and the CPU disagree on away from the shadows' edges (a pixel whose
/// neighbors the CPU sees differently is on an edge), and the shadowed pixels.
fn compare(f: &ShadowFrame, want: &[Option<bool>]) -> (usize, usize) {
    let at = |x: i32, y: i32| {
        (x >= 0 && y >= 0 && x < SIZE as i32 && y < SIZE as i32)
            .then(|| want[(y as u32 * SIZE + x as u32) as usize])
            .flatten()
    };
    let (mut wrong, mut shadowed) = (0, 0);
    for p in 0..(SIZE * SIZE) as usize {
        let Some(hidden) = want[p] else { continue };
        let (x, y) = ((p as u32 % SIZE) as i32, (p as u32 / SIZE) as i32);
        let edge =
            (-1..=1).any(|dy| (-1..=1).any(|dx| at(x + dx, y + dy).is_some_and(|h| h != hidden)));
        shadowed += usize::from(hidden);
        if !edge && (f.mask[p] < 0.5) != hidden {
            wrong += 1;
        }
    }
    (wrong, shadowed)
}

#[test]
fn the_shadow_mask_matches_rays_to_the_sun() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let (boxes, _) = scene(&mut h);
    settle(&mut h);
    let f = h.shadow_frame();
    let want = expected(&f, &boxes, Environment::default().light_direction);
    let (wrong, shadowed) = compare(&f, &want);
    assert!(shadowed > 50, "{shadowed} shadowed pixels to look at");
    assert_eq!(wrong, 0, "pixels off the edges the mask gets wrong");
    // The ground in the open is lit, not shadowed by itself (acne).
    let lit_ground = want
        .iter()
        .zip(&f.mask)
        .filter(|(w, _)| **w == Some(false))
        .all(|(_, &m)| m > 0.0);
    assert!(lit_ground, "no texel of open ground fully shadowed");
}

#[test]
fn the_shadow_follows_the_sun_and_the_box() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let (mut boxes, ids) = scene(&mut h);
    settle(&mut h);
    // The sun from the other side, the box moved: the next frame's shadow is the new one
    // (nothing is kept across frames yet).
    let light = Vec3::new(0.7, -1.0, 0.2);
    h.updates.environment = Some(Environment {
        light_direction: light,
        ..Environment::default()
    });
    boxes[1].center = Vec3::new(-0.6, 0.5, 0.3);
    h.set(
        ids[1],
        InstanceUpdate::Transform(Transform {
            translation: boxes[1].center,
            rotation: Quat::IDENTITY,
            scale: boxes[1].half * 2.0,
        }),
    );
    let f = h.shadow_frame();
    let want = expected(&f, &boxes, light);
    let (wrong, shadowed) = compare(&f, &want);
    assert!(shadowed > 100, "{shadowed} shadowed pixels to look at");
    assert_eq!(wrong, 0, "pixels off the edges the mask gets wrong");
}

#[test]
fn only_the_pages_the_surfaces_sample_are_mapped() {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    scene(&mut h);
    settle(&mut h);
    let f = h.shadow_frame();
    let mapped = f.page_table.iter().filter(|&&p| p != u32::MAX).count();
    let per_level = (SHADOW_SETTINGS.pages_per_side() * SHADOW_SETTINGS.pages_per_side()) as usize;
    assert_eq!(
        f.page_table.len(),
        per_level * SHADOW_SETTINGS.levels as usize
    );
    assert!(
        mapped > 0 && mapped as u32 == f.marked,
        "{mapped} mapped, {} marked",
        f.marked
    );
    assert!(
        mapped < f.page_table.len() / 2,
        "{mapped} of {}",
        f.page_table.len()
    );
    // Every mapped page has its own physical page.
    let mut physical: Vec<u32> = f
        .page_table
        .iter()
        .copied()
        .filter(|&p| p != u32::MAX)
        .collect();
    physical.sort_unstable();
    physical.dedup();
    assert_eq!(physical.len(), mapped);
    assert!(
        physical
            .iter()
            .all(|&p| p < SHADOW_SETTINGS.pool_pages[0] * SHADOW_SETTINGS.pool_pages[1])
    );
}
