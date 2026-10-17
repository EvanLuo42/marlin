//! World, query and schedule timings. `cargo xtask test --tier bench -p marlin-ecs`, or
//! `cargo bench -p marlin-ecs --bench world -- [filter]`.

use std::hint::black_box;

use marlin_bench::Bench;
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

const SPAWN: usize = 100_000;
const ITER: usize = 1_000_000;

fn bundle(i: usize) -> (Pos, Vel) {
    let f = i as f32;
    (Pos(f, f, f), Vel(1.0, 0.5, 0.25))
}

fn spawn() {
    Bench::new("spawn/one_by_one")
        .throughput_items(SPAWN as u64, "entity")
        .run_with_setup(World::new, |mut world| {
            for i in 0..SPAWN {
                world.spawn(bundle(i));
            }
            black_box(world);
        });

    Bench::new("spawn/batch")
        .throughput_items(SPAWN as u64, "entity")
        .run_with_setup(World::new, |mut world| {
            black_box(world.spawn_batch((0..SPAWN).map(bundle)));
        });

    Bench::new("spawn/commands_apply")
        .throughput_items(SPAWN as u64, "entity")
        .run_with_setup(World::new, |mut world| {
            let mut queue = CommandQueue::new();
            let mut commands = Commands::new(&mut queue, &world);
            for i in 0..SPAWN {
                commands.spawn(bundle(i));
            }
            world.apply(&mut queue);
            black_box(world);
        });

    Bench::new("despawn")
        .throughput_items(SPAWN as u64, "entity")
        .run_with_setup(
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

fn iterate() {
    let mut world = movers(0);
    Bench::new("iterate/uncached")
        .throughput_items(ITER as u64, "entity")
        .run(|| world.query::<(&mut Pos, &Vel)>().for_each(step));

    let mut state: QueryState<(&mut Pos, &Vel)> = world.query_state();
    Bench::new("iterate/query_state")
        .throughput_items(ITER as u64, "entity")
        .run(|| state.iter_mut(&mut world).for_each(step));

    Bench::new("iterate/query_state_for_loop")
        .throughput_items(ITER as u64, "entity")
        .run(|| {
            for item in state.iter_mut(&mut world) {
                step(item);
            }
        });

    // Where the cache matters: a small query in a world of many archetypes, every frame.
    let mut world = movers(1023);
    let frames = 1000;
    Bench::new("empty_query_1024_archetypes/uncached")
        .throughput_items(frames, "query")
        .run(|| {
            for _ in 0..frames {
                black_box(world.query_filtered::<&Pos, With<Tag>>().count());
            }
        });
    let mut state: QueryState<&Pos, With<Tag>> = world.query_state();
    Bench::new("empty_query_1024_archetypes/query_state")
        .throughput_items(frames, "query")
        .run(|| {
            for _ in 0..frames {
                black_box(state.iter(&world).count());
            }
        });
}

fn migrate() {
    let setup = || {
        let mut world = World::new();
        world.register_component_with::<SparseTag>(StorageType::SparseSet);
        let es = world.spawn_batch((0..SPAWN).map(bundle));
        (world, es)
    };
    Bench::new("migrate/insert_remove_table_tag")
        .throughput_items(2 * SPAWN as u64, "op")
        .run_with_setup(setup, |(mut world, es)| {
            for &e in &es {
                world.insert(e, Tag);
            }
            for &e in &es {
                world.remove::<Tag>(e);
            }
            black_box(world);
        });

    Bench::new("migrate/insert_remove_sparse_tag")
        .throughput_items(2 * SPAWN as u64, "op")
        .run_with_setup(setup, |(mut world, es)| {
            for &e in &es {
                world.insert(e, SparseTag);
            }
            for &e in &es {
                world.remove::<SparseTag>(e);
            }
            black_box(world);
        });
}

fn schedule() {
    let jobs = JobSystem::new(Config::default()).unwrap();
    let mut world = World::new();
    world.spawn_batch((0..1000).map(bundle));
    let runs = 1000;

    fn empty() {}
    let mut schedule = Schedule::new();
    schedule.add_system(empty);
    schedule.run(&mut world, &jobs);
    Bench::new("schedule/1_empty_system")
        .throughput_items(runs, "run")
        .run(|| {
            for _ in 0..runs {
                schedule.run(&mut world, &jobs);
            }
        });

    // 100 systems that conflict pairwise: 100 waves, run inline.
    let mut serial = Schedule::new();
    for _ in 0..100 {
        serial.add_system(|q: Query<&mut Pos>| {
            black_box(&q);
        });
    }
    serial.run(&mut world, &jobs);
    Bench::new("schedule/100_conflicting_systems")
        .throughput_items(runs, "run")
        .run(|| {
            for _ in 0..runs {
                serial.run(&mut world, &jobs);
            }
        });

    // 100 systems that share one wave: run as parallel jobs.
    let mut parallel = Schedule::new();
    for _ in 0..100 {
        parallel.add_system(|q: Query<&Pos>| {
            black_box(&q);
        });
    }
    parallel.run(&mut world, &jobs);
    Bench::new("schedule/100_parallel_systems")
        .throughput_items(runs, "run")
        .run(|| {
            for _ in 0..runs {
                parallel.run(&mut world, &jobs);
            }
        });
}

marlin_bench::main!(spawn, iterate, migrate, schedule);
