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

/// Whether the ray from `origin` along `dir` meets `b` beyond a centimeter (a surface point
/// rebuilt from far depth can sit a little inside its own surface).
fn hits(b: &Box3, origin: Vec3, dir: Vec3) -> bool {
    let (mut near, mut far) = (1e-2f32, f32::INFINITY);
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

/// Whether pixel `p` is on a shadow's edge: a neighbor the CPU sees differently.
fn on_edge(want: &[Option<bool>], p: usize) -> bool {
    let at = |x: i32, y: i32| {
        (x >= 0 && y >= 0 && x < SIZE as i32 && y < SIZE as i32)
            .then(|| want[(y as u32 * SIZE + x as u32) as usize])
            .flatten()
    };
    let (x, y) = ((p as u32 % SIZE) as i32, (p as u32 / SIZE) as i32);
    (-1..=1).any(|dy| (-1..=1).any(|dx| at(x + dx, y + dy).is_some_and(|h| Some(h) != want[p])))
}

/// Pixels the GPU and the CPU disagree on away from the shadows' edges, and the shadowed
/// pixels.
fn compare(f: &ShadowFrame, want: &[Option<bool>]) -> (usize, usize) {
    let (mut wrong, mut shadowed) = (0, 0);
    for p in 0..(SIZE * SIZE) as usize {
        let Some(hidden) = want[p] else { continue };
        shadowed += usize::from(hidden);
        if !on_edge(want, p) && (f.mask[p] < 0.5) != hidden {
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
    // The ground in the open, off the shadows' edges, is lit, not shadowed by itself (acne).
    let want = expected(&f, &boxes, light, camera());
    let lit_ground = (0..want.len())
        .filter(|&p| want[p] == Some(false) && !on_edge(&want, p))
        .all(|p| f.mask[p] > 0.0);
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

/// The fraction of a sun of angular diameter `diameter` (radians) that the ground seen at each
/// pixel from `eye` sees past `boxes`: 128 rays over the sun's disk. `None` off the ground.
fn visible_fraction(
    f: &ShadowFrame,
    boxes: &[Box3],
    light: Vec3,
    diameter: f32,
    (eye, cam): (Transform, Camera),
) -> Vec<Option<f32>> {
    let vp =
        Mat4::perspective_infinite_rh(cam.fov_y, 1.0, cam.near) * eye.to_matrix().inverse_affine();
    let inverse = vp.inverse();
    let toward = -light.normalize();
    let right = toward.cross(Vec3::Y).normalize();
    let up = right.cross(toward);
    let tan = (diameter * 0.5).tan();
    const RAYS: usize = 128;
    let dirs: Vec<Vec3> = (0..RAYS)
        .map(|r| {
            let radius = ((r as f32 + 0.5) / RAYS as f32).sqrt() * tan;
            let angle = r as f32 * 2.399_963;
            (toward + right * (radius * angle.cos()) + up * (radius * angle.sin())).normalize()
        })
        .collect();
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
            // The ground only: the boxes' own faces turned from the sun get no sun anyway.
            if (world.y + 0.5).abs() > 0.02 {
                return None;
            }
            let free = dirs
                .iter()
                .filter(|&&d| !boxes.iter().any(|b| hits(b, world, d)))
                .count();
            Some(free as f32 / RAYS as f32)
        })
        .collect()
}

/// A ground slab and a box `lift` above it under a sun shining toward the camera, of angular diameter `degrees`: the
/// boxes, the environment and the frame.
fn soft_frame(degrees: f32, lift: f32) -> (Vec<Box3>, Environment, ShadowFrame) {
    let env = Environment {
        sun_angular_diameter: degrees.to_radians(),
        // Toward the camera: the shadows fall in front of the box.
        light_direction: Vec3::new(0.5, -1.0, 0.5),
        ..Environment::default()
    };
    let mut h = Harness::new(env);
    h.vis.lod_error_pixels = 0.0;
    h.camera = Some(camera());
    let boxes = vec![
        Box3 {
            center: Vec3::new(0.0, -0.6, 0.0),
            half: Vec3::new(4.0, 0.1, 4.0),
        },
        Box3 {
            center: Vec3::new(-0.5, -0.15 + lift, -0.7),
            half: Vec3::splat(0.35),
        },
    ];
    for b in &boxes {
        h.add(
            h.cube,
            Transform {
                translation: b.center,
                rotation: Quat::IDENTITY,
                scale: b.half * 2.0,
            },
            [1.0; 4],
        );
    }
    settle(&mut h);
    let f = h.shadow_frame();
    (boxes, env, f)
}

/// Pixels of the surfaces in a penumbra, by the fractions `fraction` gives.
fn penumbra(want: &[Option<f32>], fraction: impl Fn(usize) -> f32) -> usize {
    (0..want.len())
        .filter(|&p| want[p].is_some())
        .filter(|&p| (0.05..0.95).contains(&fraction(p)))
        .count()
}

#[test]
fn soft_shadows_match_the_sun_disk_and_harden_at_contact() {
    let mut widths = Vec::new();
    for lift in [0.0, 1.5] {
        let (boxes, env, f) = soft_frame(10.0, lift);
        let want = visible_fraction(
            &f,
            &boxes,
            env.light_direction,
            env.sun_angular_diameter,
            camera(),
        );
        // Against the CPU's fractions: close on average, and in the open and the umbra alike.
        let (mut error, mut count, mut worst_solid) = (0.0f32, 0usize, 0.0f32);
        for (p, w) in want.iter().enumerate() {
            let Some(w) = *w else { continue };
            let e = (f.mask[p] - w).abs();
            error += e;
            count += 1;
            if (w == 0.0 || w == 1.0) && !on_soft_edge(&want, p) {
                worst_solid = worst_solid.max(e);
            }
        }
        let mean = error / count as f32;
        assert!(mean < 0.04, "lift {lift}: mean error {mean}");
        assert!(
            worst_solid <= 0.25,
            "lift {lift}: worst error off the penumbra {worst_solid}"
        );
        assert!(
            want.iter().filter(|w| **w == Some(0.0)).count() > 20,
            "lift {lift}: an umbra to look at"
        );
        let cpu = penumbra(&want, |p| want[p].unwrap());
        let gpu = penumbra(&want, |p| f.mask[p]);
        widths.push((cpu, gpu));
    }
    // Contact hardening: the lifted box's penumbra is wider than the resting one's.
    let [(cpu_resting, gpu_resting), (cpu_lifted, gpu_lifted)] = widths[..] else {
        unreachable!()
    };
    assert!(
        cpu_lifted > 2 * cpu_resting,
        "the reference: {cpu_resting} and {cpu_lifted}"
    );
    assert!(
        gpu_lifted > 2 * gpu_resting && gpu_resting > 0,
        "penumbra pixels: resting {gpu_resting}, lifted {gpu_lifted}"
    );
}

/// Whether a neighbor of pixel `p` has a different fraction: next to a penumbra, where one
/// pixel's shift of the reference matters.
fn on_soft_edge(want: &[Option<f32>], p: usize) -> bool {
    let (x, y) = ((p as u32 % SIZE) as i32, (p as u32 / SIZE) as i32);
    (-1..=1).any(|dy| {
        (-1..=1).any(|dx| {
            let (nx, ny) = (x + dx, y + dy);
            nx >= 0
                && ny >= 0
                && nx < SIZE as i32
                && ny < SIZE as i32
                && want[(ny as u32 * SIZE + nx as u32) as usize].is_some_and(|w| Some(w) != want[p])
        })
    })
}

#[test]
fn a_sun_without_size_casts_hard_shadows() {
    let (boxes, env, f) = soft_frame(0.0, 1.0);
    let want = expected(&f, &boxes, env.light_direction, camera());
    let (wrong, shadowed) = compare(&f, &want);
    assert!(shadowed > 50, "{shadowed} shadowed pixels to look at");
    assert_eq!(wrong, 0);
    assert!(
        f.mask
            .iter()
            .all(|&m| m == 0.0 || m == 1.0 || m.fract() != 0.0)
    );
}
