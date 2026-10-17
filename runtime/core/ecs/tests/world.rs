//! Entities, components and resources, checked against a reference model.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_ecs::{Entity, World};
use marlin_math::Rng;
use marlin_reflect::Reflect;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32, f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32, f32);

#[derive(Debug, Clone, PartialEq)]
struct Name(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Marker;

#[test]
fn spawn_get_despawn() {
    let mut world = World::new();
    let a = world.spawn((Pos(1.0, 2.0), Vel(3.0, 4.0)));
    let b = world.spawn((Pos(5.0, 6.0),));
    assert_eq!(world.get::<Pos>(a), Some(&Pos(1.0, 2.0)));
    assert_eq!(world.get::<Vel>(b), None);
    assert!(world.has::<Vel>(a) && !world.has::<Vel>(b));
    world.get_mut::<Pos>(b).unwrap().0 = 9.0;
    assert_eq!(world.get::<Pos>(b), Some(&Pos(9.0, 6.0)));
    assert_eq!(world.entity_count(), 2);

    assert!(world.despawn(a));
    assert!(!world.despawn(a), "already gone");
    assert!(!world.is_alive(a) && world.is_alive(b));
    assert_eq!(world.get::<Pos>(a), None);
    assert_eq!(world.entity_count(), 1);
}

#[test]
fn stale_ids_never_alias_reused_slots() {
    let mut world = World::new();
    let old = world.spawn((Marker,));
    world.despawn(old);
    let new = world.spawn((Marker,));
    assert_eq!(old.index(), new.index(), "the slot is reused");
    assert_ne!(old.generation(), new.generation());
    assert!(!world.is_alive(old) && world.is_alive(new));
    assert_eq!(world.get::<Marker>(old), None);
    assert_eq!(Entity::from_bits(new.to_bits()), new);
}

#[test]
fn insert_and_remove_move_between_archetypes() {
    let mut world = World::new();
    let e = world.spawn((Pos(0.0, 0.0),));
    let other = world.spawn((Pos(7.0, 7.0),));
    assert!(world.insert(e, Vel(1.0, 1.0)));
    assert_eq!(world.get::<Vel>(e), Some(&Vel(1.0, 1.0)));
    assert_eq!(world.get::<Pos>(e), Some(&Pos(0.0, 0.0)), "carried over");
    assert_eq!(
        world.get::<Pos>(other),
        Some(&Pos(7.0, 7.0)),
        "the swapped-in row stays correct"
    );

    assert!(world.insert(e, Vel(2.0, 2.0)), "replacing in place");
    assert_eq!(world.get::<Vel>(e), Some(&Vel(2.0, 2.0)));

    assert_eq!(world.remove::<Vel>(e), Some(Vel(2.0, 2.0)));
    assert_eq!(world.remove::<Vel>(e), None);
    assert_eq!(world.remove::<Name>(e), None, "never registered");
    assert_eq!(world.get::<Pos>(e), Some(&Pos(0.0, 0.0)));

    let empty = world.spawn_empty();
    world.insert(empty, Name("late".into()));
    assert_eq!(world.get::<Name>(empty).map(|n| n.0.as_str()), Some("late"));
    world.despawn(e);
    assert!(!world.insert(e, Marker), "dead entities take nothing");
}

static DROPS: AtomicUsize = AtomicUsize::new(0);

struct Counted(#[allow(dead_code)] u32);

impl Drop for Counted {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn every_component_is_dropped_exactly_once() {
    DROPS.store(0, Ordering::SeqCst);
    {
        let mut world = World::new();
        let entities: Vec<Entity> = (0..100)
            .map(|i| world.spawn((Counted(i), Pos(0.0, 0.0))))
            .collect();
        // Despawn drops.
        for &e in &entities[..10] {
            world.despawn(e);
        }
        assert_eq!(DROPS.load(Ordering::SeqCst), 10);
        // Moving between archetypes neither drops nor duplicates.
        for &e in &entities[10..30] {
            world.insert(e, Vel(0.0, 0.0));
            world.remove::<Pos>(e);
        }
        assert_eq!(DROPS.load(Ordering::SeqCst), 10);
        // Replacing drops the old value; removing hands it back.
        world.insert(entities[30], Counted(999));
        assert_eq!(DROPS.load(Ordering::SeqCst), 11);
        let taken = world.remove::<Counted>(entities[31]);
        assert!(taken.is_some());
        drop(taken);
        assert_eq!(DROPS.load(Ordering::SeqCst), 12);
        // The remaining 89 (88 originals plus the replacement) go with the world.
    }
    assert_eq!(
        DROPS.load(Ordering::SeqCst),
        101,
        "100 spawned plus one replacement"
    );
}

#[test]
fn zero_sized_components() {
    let mut world = World::new();
    let es: Vec<Entity> = (0..1000).map(|_| world.spawn((Marker,))).collect();
    assert!(es.iter().all(|&e| world.get::<Marker>(e) == Some(&Marker)));
    world.despawn(es[500]);
    assert_eq!(world.query::<&Marker>().count(), 999);
}

#[test]
#[should_panic(expected = "twice")]
fn duplicate_components_in_a_bundle_panic() {
    World::new().spawn((Pos(0.0, 0.0), Pos(1.0, 1.0)));
}

#[test]
fn resources() {
    let mut world = World::new();
    assert!(world.resource::<u32>().is_none());
    world.insert_resource(5u32);
    world.insert_resource(String::from("hi"));
    *world.resource_mut::<u32>().unwrap() += 1;
    assert_eq!(world.resource::<u32>(), Some(&6));
    world.insert_resource(10u32);
    assert_eq!(world.resource::<u32>(), Some(&10), "replaced");
    assert_eq!(world.remove_resource::<String>().as_deref(), Some("hi"));
    assert!(!world.has_resource::<String>() && world.has_resource::<u32>());
}

#[derive(Reflect, Default, Clone)]
struct Health {
    #[reflect(range = 0.0..=100.0)]
    current: f32,
    max: f32,
}

#[test]
fn reflected_components_carry_their_type_info() {
    let mut world = World::new();
    let id = world.register_reflected::<Health>();
    world.spawn((Health {
        current: 50.0,
        max: 100.0,
    },));
    let info = world.component_info(id);
    assert!(info.name().ends_with("Health"));
    assert!(info.reflect().is_some());
    let plain = world.register_component::<Pos>();
    assert!(world.component_info(plain).reflect().is_none());
}

/// What the model expects an entity to hold.
type Expected = (Option<Pos>, Option<Vel>, Option<Name>);

/// Random operations against a plain map of what each entity should hold.
#[test]
fn random_operations_match_a_model() {
    let mut world = World::new();
    let mut model: HashMap<Entity, Expected> = HashMap::new();
    let mut rng = Rng::new(0x2545_F491_4F6C_DD1D);
    let mut next = |n: u64| rng.next_u64() % n;
    for step in 0..20_000 {
        let alive: Vec<Entity> = model.keys().copied().collect();
        let pick = |r: u64| alive[(r as usize) % alive.len()];
        let f = step as f32;
        match next(7) {
            0 | 1 => {
                let e = match next(3) {
                    0 => world.spawn((Pos(f, f),)),
                    1 => world.spawn((Pos(f, 0.0), Vel(0.0, f))),
                    _ => world.spawn((Name(step.to_string()), Vel(f, f))),
                };
                let mut s = (None, None, None);
                if let Some(p) = world.get::<Pos>(e) {
                    s.0 = Some(*p);
                }
                if let Some(v) = world.get::<Vel>(e) {
                    s.1 = Some(*v);
                }
                if let Some(n) = world.get::<Name>(e) {
                    s.2 = Some(n.clone());
                }
                model.insert(e, s);
            }
            2 if !alive.is_empty() => {
                let e = pick(next(1 << 30));
                assert!(world.despawn(e));
                model.remove(&e);
            }
            3 if !alive.is_empty() => {
                let e = pick(next(1 << 30));
                world.insert(e, Vel(f, -f));
                model.get_mut(&e).unwrap().1 = Some(Vel(f, -f));
            }
            4 if !alive.is_empty() => {
                let e = pick(next(1 << 30));
                assert_eq!(world.remove::<Pos>(e), model.get_mut(&e).unwrap().0.take());
            }
            5 if !alive.is_empty() => {
                let e = pick(next(1 << 30));
                world.insert(e, Name(format!("n{step}")));
                model.get_mut(&e).unwrap().2 = Some(Name(format!("n{step}")));
            }
            _ if !alive.is_empty() => {
                let e = pick(next(1 << 30));
                assert_eq!(world.remove::<Name>(e), model.get_mut(&e).unwrap().2.take());
            }
            _ => {}
        }
    }
    assert_eq!(world.entity_count(), model.len());
    for (&e, (p, v, n)) in &model {
        assert_eq!(world.get::<Pos>(e), p.as_ref());
        assert_eq!(world.get::<Vel>(e), v.as_ref());
        assert_eq!(world.get::<Name>(e), n.as_ref());
    }
    // Queries agree with the model too.
    assert_eq!(
        world.query::<&Pos>().count(),
        model.values().filter(|s| s.0.is_some()).count()
    );
    assert_eq!(
        world.query::<(&Vel, &Name)>().count(),
        model
            .values()
            .filter(|s| s.1.is_some() && s.2.is_some())
            .count()
    );
}
