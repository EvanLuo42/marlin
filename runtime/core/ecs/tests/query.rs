//! Queries, filters and parallel iteration.

use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_ecs::{Entity, With, Without, World};
use marlin_jobs::{Config, JobSystem};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Frozen;

fn world() -> (World, Vec<Entity>) {
    let mut world = World::new();
    let mut es = Vec::new();
    for i in 0..30 {
        let f = i as f32;
        es.push(match i % 3 {
            0 => world.spawn((Pos(f),)),
            1 => world.spawn((Pos(f), Vel(1.0))),
            _ => world.spawn((Pos(f), Vel(1.0), Frozen)),
        });
    }
    (world, es)
}

#[test]
fn reads_writes_and_filters() {
    let (mut world, _) = world();
    assert_eq!(world.query::<&Pos>().count(), 30);
    assert_eq!(world.query::<(&Pos, &Vel)>().count(), 20);
    assert_eq!(world.query_filtered::<&Pos, Without<Vel>>().count(), 10);
    assert_eq!(
        world
            .query_filtered::<&Pos, (With<Vel>, Without<Frozen>)>()
            .count(),
        10
    );

    for (pos, vel) in world.query_filtered::<(&mut Pos, &Vel), Without<Frozen>>() {
        pos.0 += vel.0 * 100.0;
    }
    let moved = world.query_ref::<&Pos>().filter(|p| p.0 >= 100.0).count();
    assert_eq!(moved, 10, "only unfrozen movers moved");

    let with_optional: Vec<(Entity, bool)> = world
        .query::<(Entity, Option<&Vel>)>()
        .map(|(e, v)| (e, v.is_some()))
        .collect();
    assert_eq!(with_optional.len(), 30);
    assert_eq!(with_optional.iter().filter(|(_, v)| *v).count(), 20);
    for (e, has_vel) in with_optional {
        assert_eq!(world.has::<Vel>(e), has_vel);
    }

    for vel in world.query::<Option<&mut Vel>>().flatten() {
        vel.0 = 2.0;
    }
    assert!(world.query_ref::<&Vel>().all(|v| v.0 == 2.0));
}

#[test]
fn unknown_components_match_nothing() {
    struct Never;
    let (mut world, _) = world();
    assert_eq!(world.query::<&Never>().count(), 0);
    assert_eq!(world.query::<(&Pos, Option<&Never>)>().count(), 30);
    assert_eq!(world.query_filtered::<&Pos, With<Never>>().count(), 0);
    assert_eq!(world.query_filtered::<&Pos, Without<Never>>().count(), 30);
}

#[test]
#[should_panic(expected = "mutably")]
fn a_query_cannot_alias_itself() {
    let (mut world, _) = world();
    let _ = world.query::<(&mut Pos, &Pos)>().count();
}

#[test]
fn par_for_each_visits_every_entity_once() {
    let jobs = JobSystem::new(Config {
        workers: 4,
        ..Config::default()
    })
    .unwrap();
    let mut world = World::new();
    for i in 0..100_000 {
        if i % 2 == 0 {
            world.spawn((Pos(0.0), Vel(i as f32)));
        } else {
            world.spawn((Pos(0.0), Vel(i as f32), Frozen));
        }
    }
    let visits = AtomicUsize::new(0);
    world.par_for_each::<(&mut Pos, &Vel), _>(&jobs, 1024, |(pos, vel)| {
        pos.0 += vel.0;
        visits.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(visits.load(Ordering::Relaxed), 100_000);
    assert!(world.query_ref::<(&Pos, &Vel)>().all(|(p, v)| p.0 == v.0));
}
