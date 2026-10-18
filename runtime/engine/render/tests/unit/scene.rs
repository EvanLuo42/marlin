//! The render scene on the CPU: handles, applying updates and merging streams.

use std::collections::HashSet;
use std::sync::Arc;

use marlin_math::{Transform, Vec3};
use marlin_render::scene::{
    Bounds, IdAllocator, InstanceDesc, InstanceId, InstanceRow, InstanceUpdate, LightDesc,
    LightKind, LightUpdate, MeshUpdate,
};
use marlin_render::{Environment, RenderScene, SceneUpdates};

fn at(x: f32) -> Transform {
    Transform::from_translation(Vec3::new(x, 0.0, 0.0))
}

fn desc(scene: &RenderScene, x: f32) -> (InstanceId, InstanceDesc) {
    let mesh = scene.ids().meshes.reserve();
    (
        scene.ids().instances.reserve(),
        InstanceDesc {
            mesh,
            transform: at(x),
            color: [1.0; 4],
            flags: 0,
        },
    )
}

#[test]
fn reserved_handles_are_unique_across_threads() {
    let ids: Arc<IdAllocator<InstanceRow>> = Arc::new(IdAllocator::new());
    let all = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                let mine: Vec<InstanceId> = (0..1000).map(|_| ids.reserve()).collect();
                all.lock().unwrap().extend(mine);
            });
        }
    });
    let all = all.into_inner().unwrap();
    let rows: HashSet<u32> = all.iter().map(|id| id.index()).collect();
    assert_eq!(rows.len(), 8000);
    assert_eq!(ids.len(), 8000);
    assert!(all.iter().all(|id| id.generation() == 0));
}

#[test]
fn removed_rows_come_back_with_a_new_generation_once_applied() {
    let mut scene = RenderScene::new();
    let (a, d) = desc(&scene, 1.0);
    let mut u = SceneUpdates::default();
    u.instances.push(a, InstanceUpdate::Add(d));
    scene.apply(&u);
    assert_eq!(scene.instance(a), Some(d));

    // Not freed until the removal is applied.
    let mut u = SceneUpdates::default();
    u.instances.push(a, InstanceUpdate::Remove);
    let b = scene.ids().instances.reserve();
    assert_ne!(b.index(), a.index());
    scene.apply(&u);
    assert_eq!(scene.instance(a), None);
    assert_eq!(scene.instance_count(), 0);

    let c = scene.ids().instances.reserve();
    assert_eq!(c.index(), a.index());
    assert_eq!(c.generation(), a.generation() + 1);
    // The stale handle changes nothing, even with its row taken again.
    let mut u = SceneUpdates::default();
    u.instances.push(
        c,
        InstanceUpdate::Add(InstanceDesc {
            transform: at(5.0),
            ..d
        }),
    );
    u.instances.push(a, InstanceUpdate::Transform(at(9.0)));
    u.instances.push(a, InstanceUpdate::Remove);
    scene.apply(&u);
    assert_eq!(scene.instance(c).map(|i| i.transform), Some(at(5.0)));
    assert_eq!(scene.instance(a), None);
    assert_eq!(scene.instance_count(), 1);
}

#[test]
fn updates_to_rows_never_added_are_ignored() {
    let mut scene = RenderScene::new();
    let (a, _) = desc(&scene, 0.0);
    let mut u = SceneUpdates::default();
    u.instances.push(a, InstanceUpdate::Transform(at(1.0)));
    u.instances.push(a, InstanceUpdate::Color([0.0; 4]));
    u.instances.push(a, InstanceUpdate::Remove);
    let light = scene.ids().lights.reserve();
    u.lights.push(light, LightUpdate::Remove);
    scene.apply(&u);
    assert_eq!(scene.instance_count(), 0);
    assert_eq!(scene.instance(a), None);
    // The ignored removals freed nothing.
    assert_ne!(scene.ids().instances.reserve().index(), a.index());
}

#[test]
fn the_previous_transform_is_last_frames() {
    let mut scene = RenderScene::new();
    let (a, d) = desc(&scene, 0.0);
    let mut u = SceneUpdates::default();
    u.instances.push(a, InstanceUpdate::Add(d));
    scene.apply(&u);
    assert_eq!(scene.previous_transform(a), Some(at(0.0)));

    let moved = |scene: &mut RenderScene, xs: &[f32]| {
        let mut u = SceneUpdates::default();
        for &x in xs {
            u.instances.push(a, InstanceUpdate::Transform(at(x)));
        }
        scene.apply(&u);
        (
            scene.instance(a).unwrap().transform,
            scene.previous_transform(a).unwrap(),
        )
    };
    assert_eq!(moved(&mut scene, &[1.0]), (at(1.0), at(0.0)));
    assert_eq!(moved(&mut scene, &[2.0, 3.0]), (at(3.0), at(1.0)));
    // At rest: the previous catches up.
    assert_eq!(moved(&mut scene, &[]), (at(3.0), at(3.0)));
    assert_eq!(moved(&mut scene, &[]), (at(3.0), at(3.0)));
}

#[test]
fn merged_streams_apply_in_order() {
    let mut scene = RenderScene::new();
    let (a, d) = desc(&scene, 0.0);
    let mut first = SceneUpdates::default();
    first.instances.push(a, InstanceUpdate::Add(d));
    first.instances.push(a, InstanceUpdate::Color([0.5; 4]));
    let mut second = SceneUpdates::default();
    second.instances.push(a, InstanceUpdate::Color([0.25; 4]));
    let env = Environment {
        ambient: [1.0; 3],
        ..Environment::default()
    };
    second.environment = Some(env);
    let mut merged = SceneUpdates::default();
    merged.append(&mut first);
    merged.append(&mut second);
    assert!(first.is_empty() && second.is_empty());
    assert_eq!(merged.instances.len(), 3);
    scene.apply(&merged);
    assert_eq!(scene.instance(a).unwrap().color, [0.25; 4]);
    assert_eq!(*scene.environment(), env);
}

#[test]
fn meshes_and_lights_follow_their_updates() {
    let mut scene = RenderScene::new();
    let mesh = scene.ids().meshes.reserve();
    let light = scene.ids().lights.reserve();
    let point = LightDesc {
        kind: LightKind::Point,
        radiance: [1.0; 3],
        position: [0.0; 3],
        direction: [0.0, -1.0, 0.0],
        range: 10.0,
        cone: [0.0; 2],
    };
    let mut u = SceneUpdates::default();
    u.meshes.push(mesh, MeshUpdate::Add);
    u.lights.push(light, LightUpdate::Add(point));
    u.lights.push(
        light,
        LightUpdate::Set(LightDesc {
            range: 20.0,
            ..point
        }),
    );
    scene.apply(&u);
    assert_eq!(scene.mesh(mesh), Some(None), "referenced, not resident");
    assert_eq!(scene.light(light).map(|l| l.range), Some(20.0));

    let mut u = SceneUpdates::default();
    u.meshes.push(mesh, MeshUpdate::Remove);
    u.lights.push(light, LightUpdate::Remove);
    scene.apply(&u);
    assert_eq!(scene.mesh(mesh), None);
    assert_eq!(scene.light(light), None);
    assert_eq!(scene.ids().meshes.reserve().index(), mesh.index());
}

#[test]
fn bounds_enclose_the_points() {
    let b = Bounds::of([[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [1.0, 1.0, 0.0]].into_iter());
    assert_eq!(b.min, [0.0; 3]);
    assert_eq!(b.max, [2.0, 1.0, 0.0]);
    assert_eq!(b.center, [1.0, 0.5, 0.0]);
    assert!((b.radius - 1.25f32.sqrt()).abs() < 1e-6);
    assert_eq!(Bounds::of(core::iter::empty()).radius, 0.0);
}
