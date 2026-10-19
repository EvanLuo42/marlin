//! Extract: the render delta carries what changed in the world since the last frame, and only
//! that.

use marlin_asset::{Uuid, Vfs};
use marlin_framework::{
    Component, DEFAULT_LIGHT_RANGE, DEFAULT_SPOT_ANGLE, Engine, Light, RenderDelta, StaticMesh,
};
use marlin_math::{Quat, Transform, Vec3};
use marlin_render::scene::{InstanceUpdate, LightKind, LightUpdate, MeshUpdate};

const MESH: Uuid = Uuid::from_u128(0x1234);
const OTHER: Uuid = Uuid::from_u128(0x5678);

fn instance_updates(delta: &RenderDelta) -> Vec<InstanceUpdate> {
    delta.updates.instances.iter().map(|(_, u)| *u).collect()
}

#[test]
fn deltas_carry_only_changes() {
    let vfs = Vfs::new();
    let mut engine = Engine::new(&vfs).unwrap();
    let red = StaticMesh {
        mesh: MESH,
        color: [1.0, 0.0, 0.0, 1.0],
    };
    let e = engine.world.spawn((Transform::IDENTITY, red));
    let delta = engine.frame(0.0, None, None);
    // The mesh, referenced for the first time, and the instance of it.
    let meshes: Vec<_> = delta.updates.meshes.iter().copied().collect();
    assert_eq!(meshes.len(), 1);
    assert_eq!(meshes[0].1, MeshUpdate::Add);
    let &[(id, InstanceUpdate::Add(desc))] =
        &delta.updates.instances.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.instances);
    };
    assert_eq!(desc.mesh, meshes[0].0);
    assert_eq!(desc.transform, Transform::IDENTITY);
    assert_eq!(engine.instance_count(), 1);

    // Nothing changed.
    let delta = engine.frame(0.0, None, None);
    assert!(delta.updates.is_empty(), "{:?}", delta.updates);

    // Moved: just the transform.
    let moved = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
    *engine.world.get_mut::<Transform>(e).unwrap() = moved;
    let delta = engine.frame(0.0, None, None);
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Transform(moved)]);
    assert_eq!(delta.updates.instances.iter().next().unwrap().0, id);

    // A new color, then a new mesh (the second mesh is added with it).
    engine.world.get_mut::<StaticMesh>(e).unwrap().color = [0.0; 4];
    let delta = engine.frame(0.0, None, None);
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Color([0.0; 4])]);
    engine.world.get_mut::<StaticMesh>(e).unwrap().mesh = OTHER;
    let delta = engine.frame(0.0, None, None);
    let &[(other, MeshUpdate::Add)] = &delta.updates.meshes.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.meshes);
    };
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Mesh(other)]);

    // The component removed, then a second entity despawned.
    engine.world.remove::<StaticMesh>(e);
    let f = engine.world.spawn((Transform::IDENTITY, red));
    let delta = engine.frame(0.0, None, None);
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0], (id, InstanceUpdate::Remove));
    assert!(matches!(updates[1].1, InstanceUpdate::Add(_)));
    engine.world.despawn(f);
    let delta = engine.frame(0.0, None, None);
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates, [(updates[0].0, InstanceUpdate::Remove)]);
    assert_eq!(engine.instance_count(), 0);
    // Removals are reported once.
    assert!(engine.frame(0.0, None, None).updates.is_empty());
}

#[test]
fn lights_follow_their_entities() {
    let vfs = Vfs::new();
    let mut engine = Engine::new(&vfs).unwrap();
    let spot = Light::from_component(&Component::SpotLight {
        color: [1.0, 0.5, 0.25],
        intensity: 4.0 * core::f32::consts::PI * 100.0,
        radius: 0.1,
        range: 0.0,
        inner_angle: 30.0,
        outer_angle: 0.0,
    })
    .unwrap();
    // Lumens to candela (Φ / 4π), zeros to their defaults, the color kept.
    assert_eq!(spot.kind, LightKind::Spot);
    for (c, want) in spot.intensity.iter().zip([100.0, 50.0, 25.0]) {
        assert!((c - want).abs() < 1e-3, "{:?}", spot.intensity);
    }
    assert_eq!(spot.range, DEFAULT_LIGHT_RANGE);
    assert!((spot.cone[0] - 30f32.to_radians().cos()).abs() < 1e-6);
    assert!((spot.cone[1] - DEFAULT_SPOT_ANGLE.to_radians().cos()).abs() < 1e-6);
    let white = Light::from_component(&Component::PointLight {
        color: [0.0; 3],
        intensity: 800.0,
        radius: 0.0,
        range: 5.0,
    })
    .unwrap();
    assert_eq!(
        white.intensity[0], white.intensity[2],
        "a black color is white"
    );
    assert!(Light::from_component(&Component::FlyCamera { speed: 1.0 }).is_none());

    // Spawned: added, at the entity, shining down its -Z.
    let at = Transform {
        translation: Vec3::new(1.0, 2.0, 3.0),
        rotation: Quat::from_rotation_x(-core::f32::consts::FRAC_PI_2),
        scale: Vec3::ONE,
    };
    let e = engine.world.spawn((at, spot));
    let delta = engine.frame(0.0, None, None);
    let &[(id, LightUpdate::Add(desc))] =
        &delta.updates.lights.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.lights);
    };
    assert_eq!(desc.position, [1.0, 2.0, 3.0]);
    assert!((Vec3::from(desc.direction) - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-5);
    assert_eq!(desc.radius, 0.1);
    assert!(engine.frame(0.0, None, None).updates.lights.is_empty());

    // Moved, then brighter: the whole light again, same row.
    engine.world.get_mut::<Transform>(e).unwrap().translation = Vec3::ZERO;
    let delta = engine.frame(0.0, None, None);
    let lights: Vec<_> = delta.updates.lights.iter().copied().collect();
    assert!(matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.position == [0.0; 3]));
    engine.world.get_mut::<Light>(e).unwrap().intensity = [1.0; 3];
    let lights: Vec<_> = engine
        .frame(0.0, None, None)
        .updates
        .lights
        .iter()
        .copied()
        .collect();
    assert!(matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.radiance == [1.0; 3]));

    // Gone: removed once.
    engine.world.despawn(e);
    let lights: Vec<_> = engine
        .frame(0.0, None, None)
        .updates
        .lights
        .iter()
        .copied()
        .collect();
    assert_eq!(lights, [(id, LightUpdate::Remove)]);
    assert!(engine.frame(0.0, None, None).updates.lights.is_empty());
}
