//! Systems: wave placement, ordering, commands and access checking.

use std::sync::{Arc, Mutex};

use marlin_ecs::{Schedule, System, World};
use marlin_jobs::{Config, JobSystem};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Health(i32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Spawned(u32);

struct Gravity(f32);
struct FrameCount(u32);

fn jobs() -> JobSystem {
    JobSystem::new(Config {
        workers: 4,
        ..Config::default()
    })
    .unwrap()
}

#[test]
fn waves_follow_declared_conflicts() {
    let mut schedule = Schedule::new();
    schedule
        .add_system(
            System::new("gravity", |_| {})
                .reads::<Gravity>()
                .writes::<Vel>(),
        )
        .add_system(
            System::new("movement", |_| {})
                .reads::<Vel>()
                .writes::<Pos>(),
        )
        .add_system(System::new("regen", |_| {}).writes::<Health>())
        .add_system(System::new("hud", |_| {}).reads::<Pos>().reads::<Health>())
        .add_system(System::exclusive("cleanup", |_, _| {}))
        .add_system(System::new("count", |_| {}).writes::<FrameCount>());
    assert_eq!(
        schedule.wave_names(),
        vec![
            vec!["gravity", "regen"],
            vec!["movement"],
            vec!["hud"],
            vec!["cleanup"],
            vec!["count"],
        ]
    );
}

#[test]
fn systems_run_in_dependency_order() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Gravity(-10.0));
    world.insert_resource(FrameCount(0));
    for i in 0..1000 {
        world.spawn((Pos(i as f32), Vel(0.0), Health(i)));
    }

    let mut schedule = Schedule::new();
    schedule
        .add_system(
            System::new("gravity", |ctx| {
                let g = ctx.world.resource::<Gravity>().unwrap().0;
                for mut vel in ctx.world.query_mut::<&mut Vel>() {
                    vel.0 += g;
                }
            })
            .reads::<Gravity>()
            .writes::<Vel>(),
        )
        .add_system(
            System::new("movement", |ctx| {
                ctx.world
                    .par_for_each::<(&mut Pos, &Vel), _>(64, |(mut p, v)| p.0 += v.0);
            })
            .reads::<Vel>()
            .writes::<Pos>(),
        )
        .add_system(
            System::new("regen", |ctx| {
                for mut h in ctx.world.query_mut::<&mut Health>() {
                    h.0 += 1;
                }
            })
            .writes::<Health>(),
        )
        .add_system(
            System::new("frame", |ctx| {
                ctx.world.resource_mut::<FrameCount>().unwrap().0 += 1
            })
            .writes::<FrameCount>(),
        );

    for _ in 0..3 {
        schedule.run(&mut world, &jobs);
    }
    // Movement always saw this frame's gravity: -10, -20, -30 applied in turn.
    for (pos, vel, health) in world.query_ref::<(&Pos, &Vel, &Health)>() {
        assert_eq!(vel.0, -30.0);
        assert_eq!(pos.0, health.0 as f32 - 3.0 - 60.0);
    }
    assert_eq!(world.resource::<FrameCount>().unwrap().0, 3);
}

#[test]
fn commands_apply_after_the_wave_in_system_order() {
    let jobs = jobs();
    let mut world = World::new();
    let seen_during_wave = Arc::new(Mutex::new(Vec::new()));

    let mut schedule = Schedule::new();
    for tag in 0..4u32 {
        let seen = seen_during_wave.clone();
        // No declared conflicts, so all four share a wave and run in parallel.
        schedule.add_system(
            System::new("spawner", move |ctx| {
                seen.lock()
                    .unwrap()
                    .push(ctx.world.query::<&Spawned>().count());
                ctx.commands.spawn((Spawned(tag),));
            })
            .reads::<Spawned>(),
        );
    }
    schedule.run(&mut world, &jobs);

    assert!(
        seen_during_wave.lock().unwrap().iter().all(|&n| n == 0),
        "deferred until the wave ends"
    );
    let order: Vec<u32> = world.query_ref::<&Spawned>().map(|s| s.0).collect();
    assert_eq!(
        order,
        [0, 1, 2, 3],
        "applied in system order, not completion order"
    );
}

#[test]
fn exclusive_systems_get_the_whole_world() {
    let jobs = jobs();
    let mut world = World::new();
    let e = world.spawn((Health(1),));
    let mut schedule = Schedule::new();
    schedule.add_system(System::exclusive("despawn_dead", move |world, _| {
        world.get_mut::<Health>(e).unwrap().0 = 0;
        world.despawn(e);
    }));
    schedule.run(&mut world, &jobs);
    assert!(!world.is_alive(e));
}

#[test]
#[should_panic(expected = "without declaring")]
fn undeclared_writes_panic() {
    let jobs = jobs();
    let mut world = World::new();
    world.spawn((Pos(0.0),));
    let mut schedule = Schedule::new();
    // Alone in its wave, so it runs on this thread and the panic reaches the test.
    schedule.add_system(
        System::new("sneaky", |ctx| {
            for mut p in ctx.world.query_mut::<&mut Pos>() {
                p.0 = 1.0;
            }
        })
        .reads::<Pos>(),
    );
    schedule.run(&mut world, &jobs);
}

#[test]
#[should_panic(expected = "without declaring")]
fn undeclared_resources_panic() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Gravity(1.0));
    let mut schedule = Schedule::new();
    schedule.add_system(System::new("peek", |ctx| {
        let _ = ctx.world.resource::<Gravity>();
    }));
    schedule.run(&mut world, &jobs);
}
