//! Access by component id: raw bytes, reflection, dynamic queries, cloning and snapshots.

use std::alloc::Layout;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_ecs::{
    ComponentDescriptor, ComponentId, DynamicQuery, Schedule, StorageType, System, World,
};
use marlin_reflect::{Reflect, Value};
use marlin_test_support::{DropCounter, DropToken};

use crate::support::jobs;

#[derive(Debug, Clone, Copy, PartialEq, Reflect, Default)]
struct Pos {
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, PartialEq, Reflect, Default)]
struct Name {
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Hidden;

/// A script-defined component: two `u32`s with no Rust type.
fn script_component(world: &mut World, storage: StorageType) -> ComponentId {
    // SAFETY: plain data without a destructor.
    let desc =
        unsafe { ComponentDescriptor::from_raw("script::Pair", Layout::new::<[u32; 2]>(), None) };
    world.register_dynamic(desc.with_storage(storage))
}

#[test]
fn raw_components_by_id() {
    for storage in [StorageType::Table, StorageType::SparseSet] {
        let mut world = World::new();
        let pair = script_component(&mut world, storage);
        assert!(world.component_info(pair).type_id().is_none());
        let e = world.spawn((Hidden,));
        let mut value = [3u32, 4];
        // SAFETY: a valid `[u32; 2]`, handed over (it is `Copy`).
        assert!(unsafe { world.insert_by_id(e, pair, value.as_mut_ptr().cast()) });
        assert!(world.has_id(e, pair));
        let read = world.get_by_id(e, pair).unwrap();
        // SAFETY: the component is a `[u32; 2]`.
        assert_eq!(unsafe { read.deref::<[u32; 2]>() }, &[3, 4]);

        world.clear_trackers();
        {
            let mut write = world.get_mut_by_id(e, pair).unwrap();
            // SAFETY: as above.
            unsafe { *write.as_mut_ptr().cast::<[u32; 2]>() = [5, 6] };
        }
        let mut changed = 0;
        let mut query = DynamicQuery::builder(&world).read(pair).build();
        query.for_each_ref(&world, |row| {
            assert!(row.is_changed(0));
            // SAFETY: as above.
            assert_eq!(unsafe { *row.get(0).unwrap().cast::<[u32; 2]>() }, [5, 6]);
            changed += 1;
        });
        assert_eq!(changed, 1);

        let mut out = [0u32; 2];
        // SAFETY: storage for one `[u32; 2]`.
        assert!(unsafe { world.take_by_id_into(e, pair, out.as_mut_ptr().cast()) });
        assert_eq!(out, [5, 6]);
        assert!(!world.remove_by_id(e, pair));
        assert!(world.has::<Hidden>(e));
    }
}

#[test]
fn reflected_access_by_id() {
    let mut world = World::new();
    let pos = world.register_reflected::<Pos>();
    let e = world.spawn((Pos { x: 1.0, y: 2.0 }, Hidden));
    {
        let mut component = world.get_mut_by_id(e, pos).unwrap();
        let mut reflected = component.reflect_mut().unwrap();
        reflected
            .reborrow()
            .field("y")
            .unwrap()
            .set(9.0f32)
            .unwrap();
    }
    assert_eq!(world.get::<Pos>(e), Some(&Pos { x: 1.0, y: 9.0 }));
    let names: Vec<&str> = world
        .reflect_components(e)
        .map(|(_, r)| r.info().fields()[0].name)
        .collect();
    assert_eq!(names, ["x"], "only reflected components are listed");
}

#[test]
fn dynamic_queries_filter_write_and_cache() {
    let mut world = World::new();
    let pos = world.register_reflected::<Pos>();
    let name = world.register_reflected::<Name>();
    let hidden = world.register_component::<Hidden>();
    for i in 0..30 {
        let e = world.spawn((Pos {
            x: i as f32,
            y: 0.0,
        },));
        if i % 2 == 0 {
            world.insert(
                e,
                Name {
                    text: format!("n{i}"),
                },
            );
        }
        if i % 3 == 0 {
            world.insert(e, Hidden);
        }
    }
    let mut query = DynamicQuery::builder(&world)
        .write(pos)
        .read_optional(name)
        .without(hidden)
        .build();
    let mut named = 0;
    let mut visited = 0;
    query.for_each(&mut world, |mut row| {
        visited += 1;
        if let Some(n) = row.reflect(1) {
            assert!(n.field("text").unwrap().as_str().unwrap().starts_with('n'));
            named += 1;
        }
        let p = row.get_mut(0).unwrap().cast::<Pos>();
        // SAFETY: term 0 is a written `Pos`.
        unsafe { (*p).y = 1.0 };
    });
    assert_eq!(visited, 20);
    assert_eq!(named, 10);
    assert_eq!(world.query_ref::<&Pos>().filter(|p| p.y == 1.0).count(), 20);

    // New archetypes are picked up.
    world.spawn((Pos::default(), Name::default(), Hidden));
    world.spawn((Pos::default(), Name::default()));
    let mut count = 0;
    query.for_each(&mut world, |_| count += 1);
    assert_eq!(count, 21);
}

#[test]
#[should_panic(expected = "is a read")]
fn read_terms_cannot_be_written() {
    let mut world = World::new();
    let pos = world.register_component::<Pos>();
    world.spawn((Pos::default(),));
    let mut query = DynamicQuery::builder(&world).read(pos).build();
    query.for_each(&mut world, |mut row| {
        row.get_mut(0);
    });
}

#[test]
#[should_panic(expected = "mutably")]
fn dynamic_queries_cannot_alias_themselves() {
    let mut world = World::new();
    let pos = world.register_component::<Pos>();
    DynamicQuery::builder(&world).write(pos).read(pos).build();
}

#[test]
fn dynamic_access_is_checked_in_systems() {
    let jobs = jobs(2);
    let mut world = World::new();
    let pos = world.register_component::<Pos>();
    let pair = script_component(&mut world, StorageType::Table);
    world.spawn((Pos::default(),));
    let mut query = DynamicQuery::builder(&world).write(pos).build();
    let declared = query.access().clone();
    let info = *world.component_info(pair);
    let visits = std::sync::Arc::new(AtomicUsize::new(0));
    let v = visits.clone();
    let mut schedule = Schedule::new();
    schedule.add_system(
        System::new("script", move |ctx| {
            ctx.world.dynamic_for_each(&mut query, |_| {
                v.fetch_add(1, Ordering::Relaxed);
            });
        })
        .with_access(&declared)
        .reads_key(info.access_key(), info.name()),
    );
    // A dynamic write of `Pos` conflicts with a typed reader of it.
    schedule.add_system(System::new("reader", |_| {}).reads::<Pos>());
    assert_eq!(schedule.wave_names().len(), 2);
    schedule.run(&mut world, &jobs);
    assert_eq!(visits.load(Ordering::Relaxed), 1);
}

#[test]
#[should_panic(expected = "without declaring")]
fn undeclared_dynamic_access_panics() {
    let jobs = jobs(2);
    let mut world = World::new();
    let pos = world.register_component::<Pos>();
    let e = world.spawn((Pos::default(),));
    let mut schedule = Schedule::new();
    schedule.add_system(
        System::new("sneaky", move |ctx| {
            ctx.world.get_mut_by_id(e, pos);
        })
        .reads::<Pos>(),
    );
    schedule.run(&mut world, &jobs);
}

#[test]
fn snapshots_and_values_round_trip() {
    let mut world = World::new();
    world.register_reflected::<Pos>();
    world.register_reflected::<Name>();
    let e = world.spawn((
        Pos { x: 1.0, y: 2.0 },
        Name {
            text: "hero".into(),
        },
        Hidden,
    ));
    let snapshot: Vec<Value> = world.snapshot(e);
    assert_eq!(snapshot.len(), 2, "the unreflected `Hidden` is skipped");
    let restored = world.spawn_empty();
    for value in snapshot {
        world.insert_value(restored, value).unwrap();
    }
    assert_eq!(world.get::<Pos>(restored), Some(&Pos { x: 1.0, y: 2.0 }));
    assert_eq!(world.get::<Name>(restored).unwrap().text, "hero");
    assert!(
        world.insert_value(restored, Value::new(7u32)).is_err(),
        "not a component"
    );
}

#[test]
fn cloned_values_insert_in_one_move() {
    let mut world = World::new();
    world.register_reflected::<Pos>();
    world.register_reflected::<Name>();
    let pos = Pos { x: 3.0, y: 4.0 };
    let name = Name {
        text: "door".into(),
    };
    let e = world.spawn_empty();
    assert!(world.insert_cloned(
        e,
        &[
            marlin_reflect::Ref::new(&pos),
            marlin_reflect::Ref::new(&name)
        ]
    ));
    assert_eq!(world.get::<Pos>(e), Some(&pos));
    assert_eq!(world.get::<Name>(e).unwrap().text, "door");
    assert_eq!(
        world.archetypes().len(),
        2,
        "empty, then Pos + Name: no stop in between"
    );
    assert_eq!(name.text, "door", "the sources are cloned, not moved");

    let f = world.spawn_empty();
    assert!(
        !world.insert_cloned(
            f,
            &[
                marlin_reflect::Ref::new(&pos),
                marlin_reflect::Ref::new(&pos)
            ]
        ),
        "a type twice"
    );
    assert!(
        !world.insert_cloned(f, &[marlin_reflect::Ref::new(&7u32)]),
        "not a component"
    );
    assert!(!world.has::<Pos>(f), "nothing is added on failure");
}

#[test]
fn entities_clone_within_and_across_worlds() {
    let mut world = World::new();
    world.register_cloneable::<Hidden>();
    world.register_reflected::<Name>();
    world.register_component_with::<Pos>(StorageType::SparseSet);
    world.register_cloneable::<Pos>();
    let e = world.spawn((Pos { x: 3.0, y: 4.0 }, Name { text: "orc".into() }, Hidden));
    let copy = world.clone_entity(e).unwrap();
    assert_ne!(copy, e);
    assert_eq!(world.get::<Pos>(copy), world.get::<Pos>(e));
    assert_eq!(world.get::<Name>(copy).unwrap().text, "orc");
    assert!(world.has::<Hidden>(copy));

    let mut other = World::new();
    let far = world.clone_entity_to(e, &mut other).unwrap();
    assert_eq!(other.get::<Name>(far).unwrap().text, "orc");
    assert_eq!(other.get::<Pos>(far), Some(&Pos { x: 3.0, y: 4.0 }));

    #[derive(Debug)]
    struct Unique;
    world.insert(e, Unique);
    assert!(world.clone_entity(e).is_none());
    assert_eq!(world.entity_count(), 2, "nothing half-cloned");
}

/// Counts its drops; clones count on the same counter.
struct Counted {
    drops: DropCounter,
    _token: DropToken,
}

impl Counted {
    fn new(drops: &DropCounter) -> Self {
        Self {
            drops: drops.clone(),
            _token: drops.token(),
        }
    }
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        Self::new(&self.drops)
    }
}

#[test]
fn failed_clones_drop_what_they_made() {
    let drops = DropCounter::new();
    let mut world = World::new();
    world.register_cloneable::<Counted>();
    #[derive(Debug)]
    struct NotCloneable;
    // Component ids follow registration order, so `Counted` is cloned before the failure.
    world.register_component::<NotCloneable>();
    let e = world.spawn((Counted::new(&drops), NotCloneable));
    assert!(world.clone_entity(e).is_none());
    assert_eq!(
        drops.dropped(),
        0,
        "a missing clone fn is found before cloning"
    );
    world.remove::<NotCloneable>(e);
    let before = drops.dropped();
    let copy = world.clone_entity(e).unwrap();
    world.despawn(copy);
    assert_eq!(drops.dropped() - before, 1);
}
