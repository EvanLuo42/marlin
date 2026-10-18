//! The GPU Scene's tables read back after the scatter, compared with the render scene.

use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{InstanceId, InstanceUpdate, LightDesc, LightKind, LightUpdate, flags};
use marlin_render::{Environment, GpuScene};
use marlin_render_graph::{Readback, RenderGraph};

use crate::support::{Harness, words};

/// Rows of the instance transform table: 24 words.
const TRANSFORM_WORDS: usize = 24;

/// The rows of a 3×4 matrix, as the scatter writes them.
fn rows(t: &Transform) -> [f32; 12] {
    let a = t.to_matrix().to_cols_array();
    core::array::from_fn(|i| a[(i % 4) * 4 + i / 4])
}

fn floats(words: &[u32]) -> Vec<f32> {
    words.iter().map(|w| f32::from_bits(*w)).collect()
}

fn read_all<'g>(g: &mut RenderGraph<'g>, gpu: GpuScene<'g>) -> [Readback; 4] {
    [
        g.readback(gpu.instance_data),
        g.readback(gpu.instance_transforms),
        g.readback(gpu.instance_bounds),
        g.readback(gpu.lights),
    ]
}

struct Tables {
    data: Vec<u32>,
    transforms: Vec<u32>,
    bounds: Vec<u32>,
    lights: Vec<u32>,
}

fn frame(h: &mut Harness) -> Tables {
    let (_, _, tickets) = h.frame_with(read_all);
    let [data, transforms, bounds, lights] = tickets.map(|t| words(&h.wait_readback(t)));
    Tables {
        data,
        transforms,
        bounds,
        lights,
    }
}

fn transforms(t: &Tables, id: InstanceId) -> (Vec<f32>, Vec<f32>) {
    let at = id.index() as usize * TRANSFORM_WORDS;
    let row = floats(&t.transforms[at..at + TRANSFORM_WORDS]);
    (row[..12].to_vec(), row[12..].to_vec())
}

#[test]
fn tables_match_the_scene_and_transforms_shift_on_the_gpu() {
    let mut h = Harness::new(Environment::default());
    let a = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
    let b = Transform {
        translation: Vec3::new(-1.0, 0.5, 0.0),
        rotation: Quat::from_rotation_y(0.3),
        scale: Vec3::splat(2.0),
    };
    let c = Transform::from_translation(Vec3::new(0.0, -4.0, 0.0));
    let first = h.add(h.cube, a, [0.25, 0.5, 0.75, 1.0]);
    let second = h.add(h.cube, b, [1.0; 4]);
    let t = frame(&mut h);

    // Instance data: mesh row, flags, color.
    let at = first.index() as usize * 8;
    assert_eq!(t.data[at], h.cube.index());
    assert_eq!(t.data[at + 1], 0);
    assert_eq!(floats(&t.data[at + 4..at + 8]), [0.25, 0.5, 0.75, 1.0]);
    // New rows: previous = current.
    assert_eq!(
        transforms(&t, first),
        (rows(&a).to_vec(), rows(&a).to_vec())
    );
    assert_eq!(
        transforms(&t, second),
        (rows(&b).to_vec(), rows(&b).to_vec())
    );
    // World bounds: the unit cube's sphere (radius √3 / 2), moved and scaled by 2.
    let at = second.index() as usize * 4;
    let sphere = floats(&t.bounds[at..at + 4]);
    let expected = 3f32.sqrt();
    assert!((sphere[3] - expected).abs() < 1e-4, "{sphere:?}");
    assert!((sphere[0] + 1.0).abs() < 1e-5 && (sphere[1] - 0.5).abs() < 1e-5);

    // Moving: the GPU shifts the old current into previous.
    h.set(first, InstanceUpdate::Transform(c));
    let t = frame(&mut h);
    assert_eq!(
        transforms(&t, first),
        (rows(&c).to_vec(), rows(&a).to_vec())
    );
    assert_eq!(h.scene.previous_transform(first), Some(a));
    assert_eq!(
        transforms(&t, second),
        (rows(&b).to_vec(), rows(&b).to_vec())
    );

    // Moving again on consecutive frames.
    h.set(first, InstanceUpdate::Transform(a));
    let t = frame(&mut h);
    assert_eq!(
        transforms(&t, first),
        (rows(&a).to_vec(), rows(&c).to_vec())
    );

    // Not moving: the previous catches up once, then nothing is written.
    let t = frame(&mut h);
    assert_eq!(
        transforms(&t, first),
        (rows(&a).to_vec(), rows(&a).to_vec())
    );
    assert_eq!(h.scene.previous_transform(first), Some(a));
    let t = frame(&mut h);
    assert_eq!(
        transforms(&t, first),
        (rows(&a).to_vec(), rows(&a).to_vec())
    );

    // Several moves in one frame: previous is the last frame's.
    h.set(first, InstanceUpdate::Transform(b));
    h.set(first, InstanceUpdate::Transform(c));
    let t = frame(&mut h);
    assert_eq!(
        transforms(&t, first),
        (rows(&c).to_vec(), rows(&a).to_vec())
    );

    // Flags and lights.
    h.set(second, InstanceUpdate::Flags(flags::SHADOW));
    let light = h.scene.ids().lights.reserve();
    let desc = LightDesc {
        kind: LightKind::Spot,
        radiance: [10.0, 20.0, 30.0],
        position: [1.0, 2.0, 3.0],
        direction: [0.0, -1.0, 0.0],
        range: 15.0,
        cone: [0.9, 0.8],
    };
    h.updates.lights.push(light, LightUpdate::Add(desc));
    let t = frame(&mut h);
    assert_eq!(t.data[second.index() as usize * 8 + 1], flags::SHADOW);
    let at = light.index() as usize * 16;
    let l = &t.lights[at..at + 16];
    assert_eq!(l[0], 2, "spot");
    assert_eq!(floats(&l[1..4]), [15.0, 0.9, 0.8]);
    assert_eq!(floats(&l[4..7]), desc.radiance);
    assert_eq!(floats(&l[8..11]), desc.position);
    assert_eq!(floats(&l[12..15]), desc.direction);
}

#[test]
fn growing_the_tables_rewrites_every_row() {
    let mut h = Harness::new(Environment::default());
    let at = |i: usize| Transform::from_translation(Vec3::new(i as f32, 0.0, 0.0));
    let ids: Vec<InstanceId> = (0..10).map(|i| h.add(h.cube, at(i), [1.0; 4])).collect();
    frame(&mut h);
    // Past the first capacity (64 rows): the tables are new buffers.
    let more: Vec<InstanceId> = (10..100).map(|i| h.add(h.cube, at(i), [1.0; 4])).collect();
    let t = frame(&mut h);
    assert!(t.transforms.len() >= 100 * TRANSFORM_WORDS);
    for (i, id) in ids.iter().chain(&more).enumerate() {
        assert_eq!(
            transforms(&t, *id),
            (rows(&at(i)).to_vec(), rows(&at(i)).to_vec())
        );
    }
}
