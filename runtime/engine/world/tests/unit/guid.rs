//! Entity guids: the index follows spawns, inserts, removals and despawns; references resolve;
//! derived guids are stable.

use std::collections::HashSet;

use marlin_world::{EntityGuid, EntityRef, GuidIndex, World, WorldKind};

use crate::world;

fn index(w: &World) -> &GuidIndex {
    w.ecs().resource::<GuidIndex>().unwrap()
}

#[test]
fn the_index_follows_the_world() {
    let mut w = world(WorldKind::Game);
    let g1 = EntityGuid(0x1111);
    let g2 = EntityGuid(0x2222);
    let a = w.ecs_mut().spawn((g1,));
    assert_eq!(w.entity_by_guid(g1), Some(a));
    assert_eq!(EntityRef(g1).resolve(index(&w)), Some(a));

    // Replacing the guid re-indexes.
    w.ecs_mut().insert(a, g2);
    assert_eq!(w.entity_by_guid(g1), None);
    assert_eq!(w.entity_by_guid(g2), Some(a));

    w.ecs_mut().remove::<EntityGuid>(a);
    assert_eq!(w.entity_by_guid(g2), None);

    let b = w.ecs_mut().spawn((g1,));
    w.ecs_mut().despawn(b);
    assert_eq!(w.entity_by_guid(g1), None);
    assert!(index(&w).is_empty());
    assert_eq!(EntityRef::NONE.resolve(index(&w)), None);
}

#[test]
fn a_duplicate_guid_keeps_the_first_entity() {
    let mut w = world(WorldKind::Game);
    let g = EntityGuid(7);
    let first = w.ecs_mut().spawn((g,));
    let second = w.ecs_mut().spawn((g,));
    assert_eq!(w.entity_by_guid(g), Some(first));
    // The second going away leaves the first indexed.
    w.ecs_mut().despawn(second);
    assert_eq!(w.entity_by_guid(g), Some(first));
}

#[test]
fn derived_guids_are_stable_and_distinct() {
    let instance = EntityGuid(0x0199_2d00_0000_7000_8000_0000_0000_0001);
    let inner = EntityGuid(0x0199_2d00_0000_7000_8000_0000_0000_0002);
    let d = EntityGuid::derive(instance, inner);
    // Part of the cooked data format: this value must never change.
    assert_eq!(d, EntityGuid(0x05ab_cd05_c9bf_1426_353f_1064_7bbd_f745));
    assert_ne!(d, EntityGuid::derive(inner, instance), "order matters");
    assert_ne!(EntityGuid::derive(instance, EntityGuid(inner.0 + 1)), d);
    // A thousand instances of one Prefab never collide.
    let mut seen = HashSet::new();
    for i in 0..1000u128 {
        assert!(seen.insert(EntityGuid::derive(EntityGuid(i + 1), inner)));
    }
}
