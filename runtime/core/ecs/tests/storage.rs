//! Sparse-set storage and batch spawning, checked against plain models.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_ecs::{Added, Changed, Entity, StorageType, With, Without, World};
use marlin_jobs::{Config, JobSystem};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

/// A frequently toggled tag.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hot;

/// Sparse data with a destructor.
#[derive(Debug, Clone, PartialEq)]
struct Label(String);

fn sparse_world() -> World {
    let mut world = World::new();
    world.register_component_with::<Hot>(StorageType::SparseSet);
    world.register_component_with::<Label>(StorageType::SparseSet);
    world
}

#[test]
fn sparse_components_change_archetypes_but_not_tables() {
    let mut world = sparse_world();
    let a = world.spawn((Pos(1.0), Vel(1.0)));
    let b = world.spawn((Pos(2.0), Vel(1.0)));
    let table_before = world.archetypes()[world.entity_archetype(a).unwrap().index()].table_id();
    assert!(world.insert(a, Hot));
    let archetype = &world.archetypes()[world.entity_archetype(a).unwrap().index()];
    assert_eq!(
        archetype.table_id(),
        table_before,
        "same table, new archetype"
    );
    assert_eq!(archetype.sparse_components().len(), 1);
    assert!(world.has::<Hot>(a) && !world.has::<Hot>(b));
    assert_eq!(world.get::<Pos>(a), Some(&Pos(1.0)));
    assert_eq!(world.get::<Pos>(b), Some(&Pos(2.0)));

    assert_eq!(world.query_filtered::<&Pos, With<Hot>>().count(), 1);
    assert_eq!(world.query_filtered::<&Pos, Without<Hot>>().count(), 1);
    assert_eq!(world.query::<(Entity, &Hot, &Pos)>().count(), 1);
    let optional: Vec<bool> = world
        .query::<(&Pos, Option<&Hot>)>()
        .map(|(_, h)| h.is_some())
        .collect();
    assert_eq!(optional.iter().filter(|&&h| h).count(), 1);

    assert_eq!(world.remove::<Hot>(a), Some(Hot));
    assert_eq!(world.remove::<Hot>(a), None);
    assert_eq!(world.query_filtered::<&Pos, With<Hot>>().count(), 0);
    assert_eq!(world.get::<Pos>(a), Some(&Pos(1.0)));
}

#[test]
fn sparse_values_are_written_tracked_and_dropped() {
    let mut world = sparse_world();
    let es: Vec<Entity> = (0..10).map(|i| world.spawn((Pos(i as f32),))).collect();
    for &e in &es[..5] {
        world.insert(e, Label(format!("{}", e.index())));
    }
    world.clear_trackers();
    for (e, mut label) in world.query::<(Entity, &mut Label)>() {
        if e.index() % 2 == 0 {
            label.0.push('!');
        }
    }
    let mut changed: Vec<Entity> = world.query_filtered::<Entity, Changed<Label>>().collect();
    changed.sort();
    assert_eq!(changed.len(), 3, "indices 0, 2 and 4");
    assert_eq!(world.query_filtered::<Entity, Added<Label>>().count(), 0);
    let late = es[9];
    world.insert(late, Label("late".into()));
    assert_eq!(
        world
            .query_filtered::<Entity, Added<Label>>()
            .collect::<Vec<_>>(),
        [late]
    );
    assert_eq!(world.get::<Label>(es[0]).unwrap().0, "0!");
    world.despawn(es[0]);
    assert_eq!(world.query::<&Label>().count(), 5);
    assert_eq!(world.removed::<Label>().collect::<Vec<_>>(), [es[0]]);
}

#[test]
fn par_for_each_reaches_sparse_components() {
    let jobs = JobSystem::new(Config {
        workers: 4,
        ..Config::default()
    })
    .unwrap();
    let mut world = sparse_world();
    for i in 0..10_000 {
        let e = world.spawn((Pos(i as f32),));
        if i % 3 == 0 {
            world.insert(e, Hot);
        }
    }
    let seen = AtomicUsize::new(0);
    world.par_for_each_filtered::<&mut Pos, With<Hot>, _>(&jobs, 256, |mut p| {
        p.0 = -1.0;
        seen.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(seen.load(Ordering::Relaxed), 3334);
    assert_eq!(
        world.query_ref::<&Pos>().filter(|p| p.0 == -1.0).count(),
        3334
    );
}

#[test]
#[should_panic(expected = "already registered with Table storage")]
fn storage_is_fixed_at_registration() {
    let mut world = World::new();
    world.spawn((Hot,));
    world.register_component_with::<Hot>(StorageType::SparseSet);
}

#[test]
fn spawn_batch_fills_one_archetype() {
    let mut world = World::new();
    let first = world.spawn((Pos(-1.0),));
    let es = world.spawn_batch((0..1000).map(|i| (Pos(i as f32), Vel(i as f32 * 2.0))));
    assert_eq!(es.len(), 1000);
    let archetype = world.entity_archetype(es[0]).unwrap();
    assert!(
        es.iter()
            .all(|&e| world.entity_archetype(e) == Some(archetype))
    );
    for (i, &e) in es.iter().enumerate() {
        assert_eq!(world.get::<Vel>(e), Some(&Vel(i as f32 * 2.0)));
    }
    assert_eq!(world.get::<Pos>(first), Some(&Pos(-1.0)));
    // Iterators without an exact size work too.
    let more = world.spawn_batch((0..100).filter(|i| i % 2 == 0).map(|i| (Pos(i as f32),)));
    assert_eq!(more.len(), 50);
    assert_eq!(world.entity_count(), 1051);
}

/// What the model expects an entity to hold.
#[derive(Default, Clone, PartialEq, Debug)]
struct Expected {
    pos: Option<Pos>,
    hot: bool,
    label: Option<Label>,
}

/// Random operations mixing table and sparse components, against a plain map.
#[test]
fn random_sparse_operations_match_a_model() {
    let mut world = sparse_world();
    let mut model: HashMap<Entity, Expected> = HashMap::new();
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    for step in 0..20_000 {
        let alive: Vec<Entity> = model.keys().copied().collect();
        let pick = |r: u64| alive[(r as usize) % alive.len()];
        let f = step as f32;
        match next(8) {
            0 | 1 => {
                let e = match next(3) {
                    0 => world.spawn((Pos(f),)),
                    1 => world.spawn((Pos(f), Hot)),
                    _ => world.spawn((Label(step.to_string()),)),
                };
                let mut expected = Expected::default();
                match world.get::<Label>(e) {
                    Some(l) => expected.label = Some(l.clone()),
                    None => {
                        expected.pos = Some(Pos(f));
                        expected.hot = world.has::<Hot>(e);
                    }
                }
                model.insert(e, expected);
            }
            _ if alive.is_empty() => {}
            2 => {
                let e = pick(next(u64::MAX));
                world.despawn(e);
                model.remove(&e);
            }
            3 | 4 => {
                let e = pick(next(u64::MAX));
                world.insert(e, Hot);
                model.get_mut(&e).unwrap().hot = true;
            }
            5 => {
                let e = pick(next(u64::MAX));
                let had = world.remove::<Hot>(e).is_some();
                let entry = model.get_mut(&e).unwrap();
                assert_eq!(had, entry.hot);
                entry.hot = false;
            }
            6 => {
                let e = pick(next(u64::MAX));
                world.insert(e, Label(format!("x{step}")));
                model.get_mut(&e).unwrap().label = Some(Label(format!("x{step}")));
            }
            _ => {
                let e = pick(next(u64::MAX));
                world.insert(e, Pos(-f));
                model.get_mut(&e).unwrap().pos = Some(Pos(-f));
            }
        }
    }
    assert_eq!(world.entity_count(), model.len());
    for (&e, expected) in &model {
        let actual = Expected {
            pos: world.get::<Pos>(e).copied(),
            hot: world.has::<Hot>(e),
            label: world.get::<Label>(e).cloned(),
        };
        assert_eq!(&actual, expected, "{e:?}");
    }
    let hot = model.values().filter(|x| x.hot).count();
    assert_eq!(world.query::<&Hot>().count(), hot);
    assert_eq!(
        world.query_filtered::<&Pos, With<Hot>>().count(),
        model.values().filter(|x| x.hot && x.pos.is_some()).count()
    );
    let mut cached = world.query_state::<(Entity, Option<&Label>), Without<Hot>>();
    assert_eq!(
        cached.iter(&world).count(),
        model.values().filter(|x| !x.hot).count()
    );
    for (e, label) in cached.iter(&world) {
        assert_eq!(label, model[&e].label.as_ref());
    }
}
