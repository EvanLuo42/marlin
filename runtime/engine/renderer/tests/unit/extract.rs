//! Extract: the render delta carries what changed in the world since the last frame, and only
//! that.

use marlin_asset::{Link, Uuid};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_render::scene::{InstanceUpdate, LightKind, LightUpdate, MeshUpdate};
use marlin_renderer::{MeshRenderer, PointLight, RenderDelta, SpotLight, spot_light_desc};
use marlin_world::Transform;

use crate::Harness;

const MESH: Uuid = Uuid::from_u128(0x1234);
const OTHER: Uuid = Uuid::from_u128(0x5678);

fn instance_updates(delta: &RenderDelta) -> Vec<InstanceUpdate> {
    delta.updates.instances.iter().map(|(_, u)| *u).collect()
}

#[test]
fn deltas_carry_only_changes() {
    let mut h = Harness::new();
    let red = MeshRenderer {
        mesh: Link::new(MESH),
        color: [1.0, 0.0, 0.0, 1.0],
    };
    let e = h.world.ecs_mut().spawn((Transform::IDENTITY, red));
    let delta = h.frame();
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
    assert_eq!(marlin_renderer::instance_count(&h.world), 1);

    // Nothing changed.
    let delta = h.frame();
    assert!(delta.updates.is_empty(), "{:?}", delta.updates);

    // Moved: just the transform.
    let moved = Transform::from_translation(DVec3::new(1.0, 2.0, 3.0));
    *h.world.ecs_mut().get_mut::<Transform>(e).unwrap() = moved;
    let delta = h.frame();
    assert_eq!(
        instance_updates(&delta),
        [InstanceUpdate::Transform(
            marlin_math::Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
        )]
    );
    assert_eq!(delta.updates.instances.iter().next().unwrap().0, id);

    // A new color, then a new mesh (the second mesh is added with it).
    h.world.ecs_mut().get_mut::<MeshRenderer>(e).unwrap().color = [0.0; 4];
    let delta = h.frame();
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Color([0.0; 4])]);
    h.world.ecs_mut().get_mut::<MeshRenderer>(e).unwrap().mesh = Link::new(OTHER);
    let delta = h.frame();
    let &[(other, MeshUpdate::Add)] = &delta.updates.meshes.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.meshes);
    };
    assert_eq!(instance_updates(&delta), [InstanceUpdate::Mesh(other)]);

    // The component removed, then a second entity despawned.
    h.world.ecs_mut().remove::<MeshRenderer>(e);
    let f = h.world.ecs_mut().spawn((Transform::IDENTITY, red));
    let delta = h.frame();
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0], (id, InstanceUpdate::Remove));
    assert!(matches!(updates[1].1, InstanceUpdate::Add(_)));
    h.world.ecs_mut().despawn(f);
    let delta = h.frame();
    let updates: Vec<_> = delta.updates.instances.iter().copied().collect();
    assert_eq!(updates, [(updates[0].0, InstanceUpdate::Remove)]);
    assert_eq!(marlin_renderer::instance_count(&h.world), 0);
    // Removals are reported once.
    assert!(h.frame().updates.is_empty());
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
    let mut h = Harness::new();
    let at = Transform::from_translation(DVec3::new(1.0, 2.0, 3.0))
        .with_rotation(Quat::from_rotation_x(-core::f32::consts::FRAC_PI_2));
    let e = h.world.ecs_mut().spawn((at, spot));
    let delta = h.frame();
    let &[(id, LightUpdate::Add(desc))] =
        &delta.updates.lights.iter().copied().collect::<Vec<_>>()[..]
    else {
        panic!("{:?}", delta.updates.lights);
    };
    assert_eq!(desc.position, [1.0, 2.0, 3.0]);
    assert!((Vec3::from(desc.direction) - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-5);
    assert_eq!(desc.radius, 0.1);
    assert!(h.frame().updates.lights.is_empty());

    // Moved, then brighter: the whole light again, same row.
    h.world
        .ecs_mut()
        .get_mut::<Transform>(e)
        .unwrap()
        .translation = DVec3::ZERO;
    let delta = h.frame();
    let lights: Vec<_> = delta.updates.lights.iter().copied().collect();
    assert!(matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.position == [0.0; 3]));
    h.world.ecs_mut().get_mut::<SpotLight>(e).unwrap().intensity = 4.0 * core::f32::consts::PI;
    let lights: Vec<_> = h.frame().updates.lights.iter().copied().collect();
    assert!(
        matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && (d.radiance[0] - 1.0).abs() < 1e-5)
    );

    // Swapped for a point light: the same row, now a point light.
    h.world.ecs_mut().remove::<SpotLight>(e);
    h.world.ecs_mut().insert(e, PointLight::default());
    let lights: Vec<_> = h.frame().updates.lights.iter().copied().collect();
    assert!(
        matches!(lights[..], [(i, LightUpdate::Set(d))] if i == id && d.kind == LightKind::Point),
        "{lights:?}"
    );

    // Gone: removed once.
    h.world.ecs_mut().despawn(e);
    let lights: Vec<_> = h.frame().updates.lights.iter().copied().collect();
    assert_eq!(lights, [(id, LightUpdate::Remove)]);
    assert!(h.frame().updates.lights.is_empty());
}

#[test]
fn every_camera_is_a_view_and_targets_pick_by_priority() {
    use marlin_renderer::{Camera, ViewTarget};
    use marlin_world::EntityGuid;

    let mut h = Harness::new();
    let main = h.world.ecs_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 2.0, 0.0)),
        Camera::default(),
        EntityGuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_00c1),
    ));
    let low = h.world.ecs_mut().spawn((
        Transform::IDENTITY,
        Camera {
            priority: -1,
            ..Camera::default()
        },
    ));
    let minimap = h.world.ecs_mut().spawn((
        Transform::IDENTITY,
        Camera {
            target: ViewTarget::Offscreen(7),
            fov_y: 30.0,
            ..Camera::default()
        },
    ));
    let delta = h.frame();
    assert_eq!(delta.views.len(), 3);
    let shown = delta.view(ViewTarget::Main).unwrap();
    assert_eq!(shown.priority, 0, "the higher of the two main cameras");
    assert_eq!(shown.transform.translation, Vec3::new(0.0, 2.0, 0.0));
    let offscreen = delta.view(ViewTarget::Offscreen(7)).unwrap();
    assert!((offscreen.camera.fov_y - 30f32.to_radians()).abs() < 1e-6);
    assert!(delta.view(ViewTarget::LocalPlayer(1)).is_none());

    // Ids hold from frame to frame (they key the views' histories).
    let ids: Vec<_> = delta.views.iter().map(|v| v.id).collect();
    let again: Vec<_> = h.frame().views.iter().map(|v| v.id).collect();
    assert_eq!(ids, again);

    // Raising the low camera's priority shows it instead.
    h.world.ecs_mut().get_mut::<Camera>(low).unwrap().priority = 5;
    let delta = h.frame();
    assert_eq!(delta.view(ViewTarget::Main).unwrap().priority, 5);
    h.world.ecs_mut().despawn(main);
    h.world.ecs_mut().despawn(minimap);
    assert_eq!(h.frame().views.len(), 1);
}
