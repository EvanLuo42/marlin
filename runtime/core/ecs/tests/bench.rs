//! Timings, ignored by default. Run with
//! `cargo test -p marlin-ecs --release --test bench -- --ignored --nocapture --test-threads 1`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use marlin_ecs::{
    CommandQueue, Commands, Entity, Query, QueryState, Schedule, StorageType, With, World,
};
use marlin_jobs::{Config, JobSystem};

#[derive(Clone, Copy)]
struct Pos(f32, f32, f32);

#[derive(Clone, Copy)]
struct Vel(f32, f32, f32);

#[derive(Clone, Copy)]
struct Tag;

#[derive(Clone, Copy)]
struct SparseTag;

/// The best of `runs` timings of `f`, which gets a fresh setup each time.
fn best<S>(runs: usize, mut setup: impl FnMut() -> S, mut f: impl FnMut(S)) -> Duration {
    (0..runs)
        .map(|_| {
            let s = setup();
            let start = Instant::now();
            f(s);
            start.elapsed()
        })
        .min()
        .unwrap()
}

fn report(what: &str, time: Duration, per: usize) {
    println!(
        "{what:<52} {:>9.3} ms  {:>7.2} ns/item",
        time.as_secs_f64() * 1e3,
        time.as_nanos() as f64 / per as f64
    );
}

const SPAWN: usize = 100_000;
const ITER: usize = 1_000_000;

fn bundle(i: usize) -> (Pos, Vel) {
    let f = i as f32;
    (Pos(f, f, f), Vel(1.0, 0.5, 0.25))
}

#[test]
#[ignore]
fn spawn() {
    let t = best(5, World::new, |mut world| {
        for i in 0..SPAWN {
            world.spawn(bundle(i));
        }
        black_box(world);
    });
    report("spawn 100k (Pos, Vel), one by one", t, SPAWN);

    let t = best(5, World::new, |mut world| {
        black_box(world.spawn_batch((0..SPAWN).map(bundle)));
    });
    report("spawn_batch 100k (Pos, Vel)", t, SPAWN);

    let t = best(5, World::new, |mut world| {
        let mut queue = CommandQueue::new();
        let mut commands = Commands::new(&mut queue, &world);
        for i in 0..SPAWN {
            commands.spawn(bundle(i));
        }
        world.apply(&mut queue);
        black_box(world);
    });
    report("Commands::spawn 100k + apply", t, SPAWN);

    let t = best(
        5,
        || {
            let mut world = World::new();
            let es = world.spawn_batch((0..SPAWN).map(bundle));
            (world, es)
        },
        |(mut world, es): (World, Vec<Entity>)| {
            for e in es {
                world.despawn(e);
            }
            black_box(world);
        },
    );
    report("despawn 100k", t, SPAWN);
}

/// A world with `ITER` movers, plus `extra` archetypes of a few non-matching entities each.
fn movers(extra: usize) -> World {
    let mut world = World::new();
    world.spawn_batch((0..ITER).map(bundle));
    for i in 0..extra {
        // A distinct archetype per `i`: one marker per bit.
        let e = world.spawn((Pos(0.0, 0.0, 0.0),));
        for bit in 0..10 {
            if i & (1 << bit) != 0 {
                insert_marker(&mut world, e, bit);
            }
        }
    }
    world
}

fn insert_marker(world: &mut World, e: Entity, bit: usize) {
    #[derive(Clone, Copy)]
    struct M<const N: usize>;
    match bit {
        0 => world.insert(e, M::<0>),
        1 => world.insert(e, M::<1>),
        2 => world.insert(e, M::<2>),
        3 => world.insert(e, M::<3>),
        4 => world.insert(e, M::<4>),
        5 => world.insert(e, M::<5>),
        6 => world.insert(e, M::<6>),
        7 => world.insert(e, M::<7>),
        8 => world.insert(e, M::<8>),
        _ => world.insert(e, M::<9>),
    };
}

fn step((mut p, v): (marlin_ecs::Mut<'_, Pos>, &Vel)) {
    p.0 += v.0;
    p.1 += v.1;
    p.2 += v.2;
}

#[test]
#[ignore]
fn iterate() {
    let mut world = movers(0);
    let t = best(
        10,
        || (),
        |_| world.query::<(&mut Pos, &Vel)>().for_each(step),
    );
    report("iterate 1M (&mut Pos, &Vel), uncached", t, ITER);

    let mut state: QueryState<(&mut Pos, &Vel)> = world.query_state();
    let t = best(10, || (), |_| state.iter_mut(&mut world).for_each(step));
    report("iterate 1M (&mut Pos, &Vel), QueryState", t, ITER);

    let t = best(
        10,
        || (),
        |_| {
            for item in state.iter_mut(&mut world) {
                step(item);
            }
        },
    );
    report("iterate 1M (&mut Pos, &Vel), QueryState, for loop", t, ITER);

    // Where the cache matters: a small query in a world of many archetypes, every frame.
    let mut world = movers(1023);
    let frames = 1000;
    let t = best(
        5,
        || (),
        |_| {
            for _ in 0..frames {
                black_box(world.query_filtered::<&Pos, With<Tag>>().count());
            }
        },
    );
    report("1000x empty query, 1024 archetypes, uncached", t, frames);
    let mut state: QueryState<&Pos, With<Tag>> = world.query_state();
    let t = best(
        5,
        || (),
        |_| {
            for _ in 0..frames {
                black_box(state.iter(&world).count());
            }
        },
    );
    report("1000x empty query, 1024 archetypes, QueryState", t, frames);
}

#[test]
#[ignore]
fn migrate() {
    let setup = || {
        let mut world = World::new();
        world.register_component_with::<SparseTag>(StorageType::SparseSet);
        let es = world.spawn_batch((0..SPAWN).map(bundle));
        (world, es)
    };
    let t = best(5, setup, |(mut world, es)| {
        for &e in &es {
            world.insert(e, Tag);
        }
        for &e in &es {
            world.remove::<Tag>(e);
        }
        black_box(world);
    });
    report("insert + remove table Tag, 100k", t, 2 * SPAWN);

    let t = best(5, setup, |(mut world, es)| {
        for &e in &es {
            world.insert(e, SparseTag);
        }
        for &e in &es {
            world.remove::<SparseTag>(e);
        }
        black_box(world);
    });
    report("insert + remove sparse Tag, 100k", t, 2 * SPAWN);
}

#[test]
#[ignore]
fn schedule() {
    let jobs = JobSystem::new(Config::default()).unwrap();
    let mut world = World::new();
    world.spawn_batch((0..1000).map(bundle));
    let runs = 1000;

    fn empty() {}
    let mut schedule = Schedule::new();
    schedule.add_system(empty);
    schedule.run(&mut world, &jobs);
    let t = best(
        5,
        || (),
        |_| {
            for _ in 0..runs {
                schedule.run(&mut world, &jobs);
            }
        },
    );
    report("schedule run, 1 empty system", t, runs);

    // 100 systems that conflict pairwise: 100 waves, run inline.
    let mut serial = Schedule::new();
    for _ in 0..100 {
        serial.add_system(|q: Query<&mut Pos>| {
            black_box(&q);
        });
    }
    serial.run(&mut world, &jobs);
    let t = best(
        5,
        || (),
        |_| {
            for _ in 0..runs {
                serial.run(&mut world, &jobs);
            }
        },
    );
    report("schedule run, 100 conflicting systems (per run)", t, runs);

    // 100 systems that share one wave: run as parallel jobs.
    let mut parallel = Schedule::new();
    for _ in 0..100 {
        parallel.add_system(|q: Query<&Pos>| {
            black_box(&q);
        });
    }
    parallel.run(&mut world, &jobs);
    let t = best(
        5,
        || (),
        |_| {
            for _ in 0..runs {
                parallel.run(&mut world, &jobs);
            }
        },
    );
    report("schedule run, 100 parallel systems (per run)", t, runs);
}
