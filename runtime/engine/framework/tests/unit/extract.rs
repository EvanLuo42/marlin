//! Extract: the render delta carries what changed in the world since the last frame, and only
//! that.

use marlin_asset::{Uuid, Vfs};
use marlin_framework::{Engine, RenderDelta, StaticMesh};
use marlin_math::{Transform, Vec3};
use marlin_render::scene::{InstanceUpdate, MeshUpdate};

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
