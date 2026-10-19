//! The sun's virtual shadow maps (R2f, R2g): the shadow mask against shadows the CPU finds by
//! casting a ray from each pixel's surface toward the sun through the scene's boxes, the pages
//! the frame marks, and the pages it keeps across frames.

use marlin_math::{Mat4, Quat, Transform, Vec3, Vec4};
use marlin_render::scene::{InstanceId, InstanceUpdate, flags};
use marlin_render::shadows::RENDER_BIT;
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

/// A camera above and in front of the origin, looking down at it, moved by `offset`.
fn camera_at(offset: Vec3) -> (Transform, Camera) {
    (
        Transform {
            translation: Vec3::new(0.0, 2.6, 3.2) + offset,
            rotation: Quat::from_rotation_x(-0.65),
            scale: Vec3::ONE,
        },
        Camera::default(),
    )
}

fn camera() -> (Transform, Camera) {
    camera_at(Vec3::ZERO)
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

/// What the CPU expects of each pixel seen from `eye`: `None` for the sky, else whether the sun
/// is hidden.
fn expected(
    f: &ShadowFrame,
    boxes: &[Box3],
    light: Vec3,
    (eye, cam): (Transform, Camera),
) -> Vec<Option<bool>> {
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

/// Checks `f`'s mask against rays to the sun `light` through `boxes`, seen from `eye`.
fn check(f: &ShadowFrame, boxes: &[Box3], light: Vec3, eye: (Transform, Camera)) {
    let want = expected(f, boxes, light, eye);
    let (wrong, shadowed) = compare(f, &want);
    assert!(shadowed > 50, "{shadowed} shadowed pixels to look at");
    assert_eq!(wrong, 0, "pixels off the edges the mask gets wrong");
}

/// Marked pages: drawn or cached.
fn marked(f: &ShadowFrame) -> u32 {
    f.drawn + f.cached
}

/// A settled harness with the box scene and its first shadow frame.
fn first_frame() -> (Harness, Vec<Box3>, Vec<InstanceId>, ShadowFrame) {
    let mut h = Harness::new(Environment::default());
    h.vis.lod_error_pixels = 0.0;
    let (boxes, ids) = scene(&mut h);
    settle(&mut h);
    let f = h.shadow_frame();
    (h, boxes, ids, f)
}

#[test]
fn the_shadow_mask_matches_rays_to_the_sun() {
    let (_, boxes, _, f) = first_frame();
    let light = Environment::default().light_direction;
    check(&f, &boxes, light, camera());
    // The ground in the open is lit, not shadowed by itself (acne).
    let want = expected(&f, &boxes, light, camera());
    let lit_ground = want
        .iter()
        .zip(&f.mask)
        .filter(|(w, _)| **w == Some(false))
        .all(|(_, &m)| m > 0.0);
    assert!(lit_ground, "no texel of open ground fully shadowed");
    // Nothing was cached yet: every marked page is drawn.
    assert!(
        f.drawn > 0 && f.cached == 0,
        "{} drawn, {} cached",
        f.drawn,
        f.cached
    );
}

#[test]
fn a_still_frame_draws_no_page_and_keeps_its_shadow() {
    let (mut h, boxes, _, first) = first_frame();
    let next = h.shadow_frame();
    assert_eq!(next.drawn, 0, "pages drawn again");
    assert_eq!(next.cached, marked(&first));
    assert_eq!(next.mask, first.mask);
    check(
        &next,
        &boxes,
        Environment::default().light_direction,
        camera(),
    );
}

#[test]
fn moving_the_camera_draws_only_the_pages_it_newly_marks() {
    let (mut h, boxes, _, first) = first_frame();
    // A step sideways: the levels follow the camera, most pages stay where they were.
    let moved = camera_at(Vec3::new(0.7, 0.0, 0.0));
    h.camera = Some(moved);
    let next = h.shadow_frame();
    assert!(next.cached > 0, "no page kept");
    assert!(
        next.drawn < marked(&next) / 2,
        "{} of {} pages drawn",
        next.drawn,
        marked(&next)
    );
    assert!(marked(&first) > 0);
    check(&next, &boxes, Environment::default().light_direction, moved);
}

#[test]
fn a_moving_box_redraws_only_the_pages_under_it() {
    let (mut h, mut boxes, ids, _) = first_frame();
    boxes[1].center = Vec3::new(0.3, 0.6, -0.4);
    h.set(
        ids[1],
        InstanceUpdate::Transform(Transform {
            translation: boxes[1].center,
            rotation: Quat::IDENTITY,
            scale: boxes[1].half * 2.0,
        }),
    );
    let next = h.shadow_frame();
    assert!(next.drawn > 0, "the box's pages drawn again");
    assert!(next.cached > 0, "pages away from the box kept");
    check(
        &next,
        &boxes,
        Environment::default().light_direction,
        camera(),
    );
    // At rest again: nothing to draw.
    let rest = h.shadow_frame();
    assert_eq!(rest.drawn, 0);
    assert_eq!(rest.mask, next.mask);
}

#[test]
fn cached_pages_outlive_frames_that_do_not_mark_them() {
    let (mut h, boxes, _, first) = first_frame();
    // Looking up at the sky marks nothing.
    let (eye, cam) = camera();
    h.camera = Some((
        Transform {
            rotation: Quat::from_rotation_x(1.2),
            ..eye
        },
        cam,
    ));
    let sky = h.shadow_frame();
    assert_eq!(marked(&sky), 0);
    h.camera = Some(camera());
    let back = h.shadow_frame();
    assert_eq!(back.drawn, 0, "pages drawn again");
    assert_eq!(back.cached, marked(&first));
    check(
        &back,
        &boxes,
        Environment::default().light_direction,
        camera(),
    );
}

#[test]
fn the_shadow_follows_the_sun_and_the_box() {
    let (mut h, mut boxes, ids, _) = first_frame();
    // The sun from the other side, the box moved: the next frame's shadow is the new one, and
    // the new sun draws every page again.
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
    assert_eq!(f.cached, 0, "pages of the old sun kept");
    let want = expected(&f, &boxes, light, camera());
    let (wrong, shadowed) = compare(&f, &want);
    assert!(shadowed > 100, "{shadowed} shadowed pixels to look at");
    assert_eq!(wrong, 0, "pixels off the edges the mask gets wrong");
}

#[test]
fn only_the_pages_the_surfaces_sample_are_mapped() {
    let (_, _, _, f) = first_frame();
    let mapped: Vec<u32> = f
        .page_table
        .iter()
        .copied()
        .filter(|&p| p != u32::MAX)
        .collect();
    let per_level = (SHADOW_SETTINGS.pages_per_side() * SHADOW_SETTINGS.pages_per_side()) as usize;
    assert_eq!(
        f.page_table.len(),
        per_level * SHADOW_SETTINGS.levels as usize
    );
    assert!(
        !mapped.is_empty() && mapped.len() as u32 == marked(&f),
        "{} mapped, {} marked",
        mapped.len(),
        marked(&f)
    );
    assert!(
        mapped.len() < f.page_table.len() / 2,
        "{} of {}",
        mapped.len(),
        f.page_table.len()
    );
    // A first frame draws every mapped page, each into its own physical page.
    assert!(mapped.iter().all(|&p| p & RENDER_BIT != 0));
    let mut physical: Vec<u32> = mapped.iter().map(|&p| p & !RENDER_BIT).collect();
    physical.sort_unstable();
    physical.dedup();
    assert_eq!(physical.len(), mapped.len());
    assert!(
        physical
            .iter()
            .all(|&p| p < SHADOW_SETTINGS.physical_pages())
    );
}

#[test]
fn a_new_color_keeps_every_page() {
    let (mut h, _, ids, first) = first_frame();
    h.set(ids[1], InstanceUpdate::Color([0.2, 0.9, 0.2, 1.0]));
    let next = h.shadow_frame();
    assert_eq!(next.drawn, 0, "pages drawn for a color");
    assert_eq!(next.mask, first.mask);
}

#[test]
fn hiding_the_box_takes_its_shadow_away() {
    let (mut h, boxes, ids, _) = first_frame();
    h.set(ids[1], InstanceUpdate::Flags(flags::HIDDEN));
    let next = h.shadow_frame();
    assert!(next.drawn > 0 && next.cached > 0);
    let want = expected(
        &next,
        &boxes[..1],
        Environment::default().light_direction,
        camera(),
    );
    assert_eq!(compare(&next, &want), (0, 0), "a shadow left behind");
}
