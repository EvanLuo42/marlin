//! Extract: the render delta carries what changed in the world since the last frame, and only
//! that.

use marlin_asset::{Link, Uuid, Vfs};
use marlin_framework::{Engine, MeshRenderer, PointLight, RenderDelta, SpotLight, spot_light_desc};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_render::scene::{InstanceUpdate, LightKind, LightUpdate, MeshUpdate};
use marlin_world::Transform;

const MESH: Uuid = Uuid::from_u128(0x1234);
const OTHER: Uuid = Uuid::from_u128(0x5678);

fn instance_updates(delta: &RenderDelta) -> Vec<InstanceUpdate> {
    delta.updates.instances.iter().map(|(_, u)| *u).collect()
}

#[test]
fn deltas_carry_only_changes() {
    let vfs = Vfs::new();
    let mut engine = Engine::new(&vfs).unwrap();
    let red = MeshRenderer {
        mesh: Link::new(MESH),
        color: [1.0, 0.0, 0.0, 1.0],
    };
    let e = engine.world.ecs_mut().spawn((Transform::IDENTITY, red));
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
    assert_eq!(desc.transform, marlin_math::Transform::IDENTITY);
    assert_eq!(engine.instance_count(), 1);

    // Nothing changed.
    let delta = engine.frame(0.0, None, None);
    assert!(delta.updates.is_empty(), "{:?}", delta.updates);

    // Moved: just the transform.
    let moved = Transform::from_translation(DVec3::new(1.0, 2.0, 3.0));
    *engine.world.ecs_mut().get_mut::<Transform>(e).unwrap() = moved;
    let delta = engine.frame(0.0, None, None);
    assert_eq!(
        instance_updates(&delta),
        [InstanceUpdate::Transform(
            marlin_math::Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
        )]
    );
    assert_eq!(delta.updates.instances.iter().next().unwrap().0, id);

    // A new color, then a new mesh (the second mesh is added with it).
    engine
        .world
        .ecs_mut()
        .get_mut::<MeshRenderer>(e)
        .unwrap()
        .color = [0.0; 4];
    let delta = engine.frame(0.0, None, None);
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Color([0.0; 4])]);
    engine
        .world
        .ecs_mut()
        .get_mut::<MeshRenderer>(e)
        .unwrap()
        .mesh = Link::new(OTHER);
    let delta = engine.frame(0.0, None, None);
    let &[(other, MeshUpdate::Add)] = &delta.updates.meshes.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.meshes);
    };
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Mesh(other)]);

    // The component removed, then a second entity despawned.
    engine.world.ecs_mut().remove::<MeshRenderer>(e);
    let f = engine.world.ecs_mut().spawn((Transform::IDENTITY, red));
    let delta = engine.frame(0.0, None, None);
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0], (id, InstanceUpdate::Remove));
    assert!(matches!(updates[1].1, InstanceUpdate::Add(_)));
    engine.world.ecs_mut().despawn(f);
    let delta = engine.frame(0.0, None, None);
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates, [(updates[0].0, InstanceUpdate::Remove)]);
    assert_eq!(engine.instance_count(), 0);
    // Removals are reported once.
    assert!(engine.frame(0.0, None, None).updates.is_empty());
}

#[test]
fn lights_follow_their_entities() {
    // Lumens to candela (Φ / 4π), angles to cosines, the color kept.
    let spot = SpotLight {
        color: [1.0, 0.5, 0.25],
        intensity: 4.0 * core::f32::consts::PI * 100.0,
        radius: 0.1,
        inner_angle: 30.0,
        ..SpotLight::default()
    };
    let desc = spot_light_desc(&spot, &marlin_math::Transform::IDENTITY);
    assert_eq!(desc.kind, LightKind::Spot);
    for (c, want) in desc.radiance.iter().zip([100.0, 50.0, 25.0]) {
        assert!((c - want).abs() < 1e-3, "{:?}", desc.radiance);
    }
    assert_eq!(desc.range, SpotLight::default().range);
    assert!((desc.cone[0] - 30f32.to_radians().cos()).abs() < 1e-6);
    assert!((desc.cone[1] - SpotLight::default().outer_angle.to_radians().cos()).abs() < 1e-6);

    // Spawned: added, at the entity, shining down its -Z.
    let vfs = Vfs::new();
    let mut engine = Engine::new(&vfs).unwrap();
    let at = Transform::from_translation(DVec3::new(1.0, 2.0, 3.0))
        .with_rotation(Quat::from_rotation_x(-core::f32::consts::FRAC_PI_2));
    let e = engine.world.ecs_mut().spawn((at, spot));
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
    engine
        .world
        .ecs_mut()
        .get_mut::<Transform>(e)
        .unwrap()
        .translation = DVec3::ZERO;
    let delta = engine.frame(0.0, None, None);
    let lights: Vec<_> = delta.updates.lights.iter().copied().collect();
    assert!(matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.position == [0.0; 3]));
    engine
        .world
        .ecs_mut()
        .get_mut::<SpotLight>(e)
        .unwrap()
        .intensity = 4.0 * core::f32::consts::PI;
    let lights: Vec<_> = engine
        .frame(0.0, None, None)
        .updates
        .lights
        .iter()
        .copied()
        .collect();
    assert!(
        matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && (d.radiance[0] - 1.0).abs() < 1e-5)
    );

    // Swapped for a point light: the same row, now a point light.
    engine.world.ecs_mut().remove::<SpotLight>(e);
    engine.world.ecs_mut().insert(e, PointLight::default());
    let lights: Vec<_> = engine
        .frame(0.0, None, None)
        .updates
        .lights
        .iter()
        .copied()
        .collect();
    assert!(
        matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.kind == LightKind::Point),
        "{lights:?}"
    );

    // Gone: removed once.
    engine.world.ecs_mut().despawn(e);
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
