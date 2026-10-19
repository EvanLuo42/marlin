//! The component registry: the world's own types, lookups by name, and installation in worlds.

use std::sync::Arc;

use marlin_ecs::StorageType;
use marlin_reflect::Reflect;
use marlin_world::{ComponentFlags, ComponentRegistry, World, WorldKind};

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "test::Health")]
struct Health {
    value: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "test::Stunned")]
struct Stunned;

struct Cache;

#[test]
fn the_world_registers_its_own_types() {
    let r = ComponentRegistry::new();
    let t = r.by_name("marlin_world::Transform").unwrap();
    assert!(t.flags.contains(ComponentFlags::SAVED));
    assert!(t.reflect.is_some());
    let g = r.by_name("marlin_world::GlobalTransform").unwrap();
    assert!(!g.flags.contains(ComponentFlags::SAVED));
    assert!(r.by_name("marlin_world::EntityGuid").is_some());
}

#[test]
fn modules_add_components_that_worlds_install() {
    let mut r = ComponentRegistry::new();
    r.add::<Health>(ComponentFlags::SAVED | ComponentFlags::REPLICATED)
        .add_with_storage::<Stunned>(ComponentFlags::TRANSIENT, StorageType::SparseSet)
        .add_runtime::<Cache>(StorageType::Table);
    let health = r.get::<Health>().unwrap();
    assert_eq!(health.name, "test::Health");
    assert!(
        health
            .flags
            .contains(ComponentFlags::SAVED | ComponentFlags::REPLICATED)
    );
    assert!(r.get::<Cache>().unwrap().reflect.is_none());

    let w = World::new(WorldKind::Game, Arc::new(r));
    let id = w.ecs().component_id::<Health>().expect("installed");
    assert!(
        w.ecs().component_info(id).reflect().is_some(),
        "with its reflection"
    );
    let stunned = w.ecs().component_id::<Stunned>().unwrap();
    assert_eq!(
        w.ecs().component_info(stunned).storage(),
        StorageType::SparseSet
    );
}

#[test]
#[should_panic(expected = "registered twice")]
fn a_type_registers_once() {
    let mut r = ComponentRegistry::new();
    r.add::<Health>(ComponentFlags::SAVED);
    r.add::<Health>(ComponentFlags::SAVED);
}
