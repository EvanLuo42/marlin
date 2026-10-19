//! Levels and Prefabs: flattening instances and overrides, cooking into archetypes, spawning,
//! streaming and unloading sublevels, spawning Prefab instances.

use std::sync::Arc;

use marlin_asset::{Link, Uuid};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Children, Entity, Parent};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_reflect::Reflect;
use marlin_world::{
    ComponentData, ComponentFlags, ComponentRegistry, CookedLevel, EntityDesc, EntityGuid,
    EntityName, FlatEntity, FlattenError, GlobalTransform, InLevel, NoPrefabs, Override, Prefab,
    PrefabInstance, SublevelState, Transform, World, WorldKind, flatten, flatten_prefab,
};

use crate::jobs;

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "level_test::Light", default)]
struct Light {
    intensity: f32,
    range: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "level_test::Door")]
struct Door {
    open: bool,
}

fn registry() -> Arc<ComponentRegistry> {
    let mut r = ComponentRegistry::new();
    r.add::<Light>(ComponentFlags::SAVED)
        .add::<Door>(ComponentFlags::SAVED);
    Arc::new(r)
}

fn world() -> World {
    World::new(WorldKind::Game, registry())
}

fn guid(n: u128) -> EntityGuid {
    EntityGuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0000 | n)
}

fn at(x: f64) -> Transform {
    Transform::from_translation(DVec3::new(x, 0.0, 0.0))
}

fn entity(
    g: EntityGuid,
    name: &str,
    parent: EntityGuid,
    components: Vec<ComponentData>,
) -> EntityDesc {
    EntityDesc {
        guid: g,
        name: name.into(),
        parent,
        components,
    }
}

const DOOR: Uuid = Uuid::from_u128(0xd00d);
const HOUSE: Uuid = Uuid::from_u128(0x4005e);

/// A door: a frame (the root, at x = 1) with a light child.
fn door() -> Prefab {
    Prefab {
        entities: vec![
            entity(
                guid(1),
                "Frame",
                EntityGuid::NIL,
                vec![
                    ComponentData::new(at(1.0)),
                    ComponentData::new(Door { open: false }),
                ],
            ),
            entity(
                guid(2),
                "Lamp",
                guid(1),
                vec![
                    ComponentData::new(at(0.5)),
                    ComponentData::new(Light {
                        intensity: 100.0,
                        range: 5.0,
                    }),
                ],
            ),
        ],
        instances: Vec::new(),
    }
}

/// A house: a root with a door instance (guid 11) whose lamp is brighter.
fn house() -> Prefab {
    Prefab {
        entities: vec![entity(
            guid(10),
            "House",
            EntityGuid::NIL,
            vec![ComponentData::new(at(0.0))],
        )],
        instances: vec![PrefabInstance {
            guid: guid(11),
            name: "FrontDoor".into(),
            parent: guid(10),
            prefab: Link::new(DOOR),
            transform: at(3.0),
            overrides: vec![Override::Set {
                entity: guid(2),
                component: ComponentData::new(Light {
                    intensity: 200.0,
                    range: 0.0,
                }),
                fields: vec!["intensity".into()],
            }],
        }],
    }
}

fn prefabs() -> HashMap<Uuid, Prefab> {
    let mut m = HashMap::new();
    m.insert(DOOR, door());
    m.insert(HOUSE, house());
    m
}

fn find(flat: &[FlatEntity], g: EntityGuid) -> &FlatEntity {
    flat.iter()
        .find(|e| e.guid == g)
        .unwrap_or_else(|| panic!("{g:?}"))
}

fn component<T: Reflect>(e: &FlatEntity) -> Option<&T> {
    e.components.iter().find_map(|c| c.downcast_ref::<T>())
}

#[test]
fn instances_expand_with_derived_guids_and_overrides() {
    let level_instance = PrefabInstance {
        guid: guid(100),
        name: String::new(),
        parent: EntityGuid::NIL,
        prefab: Link::new(HOUSE),
        transform: at(50.0),
        overrides: vec![Override::Set {
            // The lamp of the house's door, as the house sees it.
            entity: EntityGuid::derive(guid(11), guid(2)),
            component: ComponentData::new(Light {
                intensity: 0.0,
                range: 9.0,
            }),
            fields: vec!["range".into()],
        }],
    };
    let flat = flatten(&[], &[level_instance], &prefabs()).unwrap();
    assert_eq!(flat.len(), 3);

    let house = find(&flat, guid(100));
    assert!(house.parent.is_nil());
    assert_eq!(
        component::<Transform>(house),
        Some(&at(50.0)),
        "the instance places the root"
    );

    let door = find(&flat, EntityGuid::derive(guid(100), guid(11)));
    assert_eq!(door.name, "FrontDoor");
    assert_eq!(door.parent, guid(100));
    assert_eq!(component::<Transform>(door), Some(&at(3.0)));

    let lamp_guid = EntityGuid::derive(guid(100), EntityGuid::derive(guid(11), guid(2)));
    let lamp = find(&flat, lamp_guid);
    assert_eq!(lamp.parent, door.guid);
    assert_eq!(
        component::<Light>(lamp),
        Some(&Light {
            intensity: 200.0,
            range: 9.0
        }),
        "the house's override, then the level's, each on its own field"
    );
}

#[test]
fn overrides_add_and_remove() {
    let instance = |overrides| PrefabInstance {
        guid: guid(100),
        prefab: Link::new(DOOR),
        transform: at(0.0),
        overrides,
        ..PrefabInstance::default()
    };
    let lamp = EntityGuid::derive(guid(100), guid(2));

    let removed = flatten(
        &[],
        &[instance(vec![Override::RemoveEntity { entity: guid(2) }])],
        &prefabs(),
    )
    .unwrap();
    assert_eq!(removed.len(), 1);

    let flat = flatten(
        &[],
        &[instance(vec![
            Override::RemoveComponent {
                entity: guid(2),
                component: "Light".into(),
            },
            Override::Set {
                entity: guid(2),
                component: ComponentData::new(Door { open: true }),
                fields: Vec::new(),
            },
        ])],
        &prefabs(),
    )
    .unwrap();
    let e = find(&flat, lamp);
    assert!(component::<Light>(e).is_none());
    assert_eq!(
        component::<Door>(e),
        Some(&Door { open: true }),
        "added whole"
    );
}

#[test]
fn bad_data_is_reported() {
    let place = |prefab: Uuid, overrides| PrefabInstance {
        guid: guid(100),
        prefab: Link::new(prefab),
        overrides,
        ..PrefabInstance::default()
    };
    assert!(matches!(
        flatten(
            &[],
            &[place(Uuid::from_u128(0xbad), Vec::new())],
            &prefabs()
        ),
        Err(FlattenError::MissingPrefab { .. })
    ));
    assert!(matches!(
        flatten(
            &[],
            &[place(
                DOOR,
                vec![Override::Set {
                    entity: guid(2),
                    component: ComponentData::new(Light::default()),
                    fields: vec!["colour".into()],
                }]
            )],
            &prefabs()
        ),
        Err(FlattenError::OverridePath { .. })
    ));
    assert!(matches!(
        flatten(
            &[],
            &[place(
                DOOR,
                vec![Override::RemoveEntity { entity: guid(77) }]
            )],
            &prefabs()
        ),
        Err(FlattenError::OverrideTarget { .. })
    ));

    let mut cyclic = prefabs();
    cyclic
        .get_mut(&DOOR)
        .unwrap()
        .instances
        .push(PrefabInstance {
            guid: guid(5),
            parent: guid(1),
            prefab: Link::new(HOUSE),
            ..PrefabInstance::default()
        });
    assert_eq!(
        flatten_prefab(HOUSE, &cyclic).unwrap_err(),
        FlattenError::Cycle(HOUSE)
    );

    let mut two_roots = prefabs();
    two_roots.get_mut(&DOOR).unwrap().entities[1].parent = EntityGuid::NIL;
    assert!(matches!(
        flatten(&[], &[place(DOOR, Vec::new())], &two_roots),
        Err(FlattenError::Roots { roots: 2, .. })
    ));

    let twice = entity(guid(1), "", EntityGuid::NIL, Vec::new());
    assert_eq!(
        flatten(&[twice.clone(), twice], &[], &NoPrefabs).unwrap_err(),
        FlattenError::DuplicateGuid(guid(1))
    );
    let doubled = entity(
        guid(1),
        "",
        EntityGuid::NIL,
        vec![ComponentData::new(at(0.0)), ComponentData::new(at(1.0))],
    );
    assert!(matches!(
        flatten(&[doubled], &[], &NoPrefabs),
        Err(FlattenError::DuplicateComponent { .. })
    ));
}

/// A level whose child comes before its parent and whose entities have two component sets.
fn level() -> CookedLevel {
    let entities = vec![
        entity(
            guid(3),
            "Child",
            guid(1),
            vec![
                ComponentData::new(Light::default()),
                ComponentData::new(at(2.0)),
            ],
        ),
        entity(
            guid(1),
            "Root",
            EntityGuid::NIL,
            vec![ComponentData::new(at(10.0))],
        ),
        entity(
            guid(2),
            "Other",
            EntityGuid::NIL,
            vec![ComponentData::new(at(-1.0))],
        ),
    ];
    CookedLevel::from_flat(
        flatten(&entities, &[], &NoPrefabs).unwrap(),
        EntityGuid::NIL,
    )
}

#[test]
fn cooked_levels_group_entities_by_archetype_and_round_trip() {
    let _ = registry();
    let cooked = level();
    assert_eq!(cooked.archetypes.len(), 2);
    assert_eq!(
        cooked.archetypes[1].entities.len(),
        2,
        "Root and Other share one"
    );
    assert_eq!(cooked.entity_count(), 3);
    let bytes = marlin_serial::to_bytes(&cooked).unwrap();
    let back: CookedLevel = marlin_serial::from_bytes(&bytes).unwrap();
    assert_eq!(back.archetypes.len(), 2);
    assert_eq!(
        back.archetypes[1].columns[0][0].downcast_ref::<Transform>(),
        Some(&at(10.0))
    );
}

fn by_guid(w: &World, g: EntityGuid) -> Entity {
    w.entity_by_guid(g)
        .unwrap_or_else(|| panic!("{g:?} is not spawned"))
}

#[test]
fn spawned_levels_are_placed_parented_and_indexed() {
    let jobs = jobs();
    let mut w = world();
    let placement = Transform::from_translation(DVec3::new(0.0, 100.0, 0.0))
        .with_rotation(Quat::from_axis_angle(Vec3::Y, core::f32::consts::PI));
    let id = w.spawn_level(Arc::new(level()), placement);
    assert_eq!(w.levels().state(id), Some(SublevelState::Loaded));
    let (root, child) = (by_guid(&w, guid(1)), by_guid(&w, guid(3)));
    assert_eq!(w.ecs().get::<Parent>(child).map(Parent::get), Some(root));
    assert_eq!(w.ecs().get::<Children>(root).unwrap().as_slice(), [child]);
    assert_eq!(w.ecs().get::<EntityName>(child).unwrap().0, "Child");
    assert_eq!(w.ecs().get::<InLevel>(child).unwrap().0, id);
    assert!(w.ecs().get::<Light>(child).is_some());

    w.frame(0.016, &jobs);
    // Roots are placed; children follow their parents.
    let g = |e| w.ecs().get::<GlobalTransform>(e).unwrap().translation;
    assert!(
        g(root).abs_diff_eq(DVec3::new(-10.0, 100.0, 0.0), 1e-4),
        "{:?}",
        g(root)
    );
    assert!(
        g(child).abs_diff_eq(DVec3::new(-12.0, 100.0, 0.0), 1e-4),
        "{:?}",
        g(child)
    );
}

#[test]
fn sublevels_stream_in_and_unload() {
    let jobs = jobs();
    let mut w = world();
    let persistent = w.spawn_level(
        Arc::new(CookedLevel::from_flat(
            flatten(
                &[entity(guid(50), "Sky", EntityGuid::NIL, Vec::new())],
                &[],
                &NoPrefabs,
            )
            .unwrap(),
            EntityGuid::NIL,
        )),
        Transform::IDENTITY,
    );

    let asset = Uuid::from_u128(0x50b);
    let id = w.load_sublevel(asset, Transform::IDENTITY);
    assert_eq!(w.levels().state(id), Some(SublevelState::Requested));
    let requests: Vec<_> = w.levels().requests().collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].level, asset);
    w.frame(0.016, &jobs);
    assert_eq!(
        w.levels().state(id),
        Some(SublevelState::Requested),
        "waits for the asset"
    );

    assert!(w.provide_level(id, Arc::new(level())));
    assert!(!w.provide_level(id, Arc::new(level())), "only once");
    w.frame(0.016, &jobs);
    assert_eq!(w.levels().state(id), Some(SublevelState::Loaded));
    assert_eq!(w.levels().entities(id).len(), 3);
    let child = by_guid(&w, guid(3));
    assert_eq!(
        w.ecs().get::<Parent>(child).map(Parent::get),
        Some(by_guid(&w, guid(1))),
        "the child spawned first is parented once its parent is there"
    );

    assert_eq!(w.unload_sublevel(id), 3);
    assert!(w.entity_by_guid(guid(1)).is_none());
    assert!(w.entity_by_guid(guid(50)).is_some(), "other levels stay");
    assert_eq!(w.levels().state(id), None);
    assert_eq!(w.levels().entities(persistent).len(), 1);

    // Unloading while waiting cancels the load.
    let pending = w.load_sublevel(asset, Transform::IDENTITY);
    assert_eq!(w.unload_sublevel(pending), 0);
    assert!(!w.provide_level(pending, Arc::new(level())));
}

#[test]
fn prefab_instances_spawn_with_their_own_guids() {
    let mut w = world();
    let cooked = CookedLevel::from_flat(flatten_prefab(DOOR, &prefabs()).unwrap(), guid(1));
    let a = w.spawn_prefab(&cooked, guid(200), at(5.0), None).unwrap();
    let b = w
        .spawn_prefab(&cooked, guid(201), at(6.0), Some(a))
        .unwrap();
    assert_eq!(by_guid(&w, guid(200)), a);
    assert_eq!(w.ecs().get::<Transform>(b), Some(&at(6.0)));
    assert_eq!(w.ecs().get::<Parent>(b).map(Parent::get), Some(a));
    let lamp_a = by_guid(&w, EntityGuid::derive(guid(200), guid(2)));
    let lamp_b = by_guid(&w, EntityGuid::derive(guid(201), guid(2)));
    assert_ne!(lamp_a, lamp_b);
    assert_eq!(w.ecs().get::<Parent>(lamp_b).map(Parent::get), Some(b));
    assert_eq!(w.ecs().get::<Transform>(lamp_a), Some(&at(0.5)));
}

#[test]
fn components_of_unknown_types_are_counted_and_left_out() {
    let mut cooked = level();
    // As a component of a type the build no longer has reads back.
    cooked.archetypes[1].columns[0][0] = ComponentData::default();
    assert_eq!(cooked.unknown_components(), 1);
    let mut w = world();
    w.spawn_level(Arc::new(cooked), Transform::IDENTITY);
    let root = by_guid(&w, guid(1));
    assert!(w.ecs().get::<Transform>(root).is_none(), "left out");
    assert!(w.ecs().get::<EntityName>(root).is_some(), "the rest spawns");
}
