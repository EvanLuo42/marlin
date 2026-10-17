//! Systems: wave placement, ordering, commands and access checking.

use std::sync::{Arc, Mutex};

use marlin_ecs::{IntoSystem, Res, ResMut, Schedule, System, World, not, resource_exists};
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

// --- Ordering, sets and conditions ---

/// A system that only records its name when it runs.
fn logger(name: &'static str, log: &Arc<Mutex<Vec<&'static str>>>) -> System {
    let log = log.clone();
    System::new(name, move |_| log.lock().unwrap().push(name))
}

#[test]
fn explicit_ordering_overrides_insertion_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut schedule = Schedule::new();
    schedule
        .add_system(logger("render", &log).after("physics"))
        .add_system(logger("physics", &log).after("input"))
        .add_system(logger("input", &log))
        .add_system(logger("audio", &log).before("render"));
    assert_eq!(
        schedule.wave_names(),
        vec![vec!["input", "audio"], vec!["physics"], vec!["render"]]
    );
    let jobs = jobs();
    let mut world = World::new();
    schedule.run(&mut world, &jobs);
    let log = log.lock().unwrap();
    let at = |n| log.iter().position(|&x| x == n).unwrap();
    assert!(at("input") < at("physics") && at("physics") < at("render"));
    assert!(at("audio") < at("render"));
}

#[test]
fn conflicts_follow_the_sorted_order() {
    // Both write `Pos`; the constraint puts the later-added one first, and the conflict keeps
    // them apart in that order.
    let mut schedule = Schedule::new();
    schedule
        .add_system(System::new("second", |_| {}).writes::<Pos>())
        .add_system(
            System::new("first", |_| {})
                .writes::<Pos>()
                .before("second"),
        )
        .add_system(System::new("free", |_| {}).writes::<Health>());
    assert_eq!(
        schedule.wave_names(),
        vec![vec!["first", "free"], vec!["second"]]
    );
}

#[test]
fn sets_order_and_nest() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut schedule = Schedule::new();
    schedule.configure_set("late").after("early");
    schedule.configure_set("physics").in_set("early");
    schedule
        .add_system(logger("draw", &log).in_set("late"))
        .add_system(logger("collide", &log).in_set("physics"))
        .add_system(
            logger("integrate", &log)
                .in_set("physics")
                .before("collide"),
        )
        .add_system(logger("ui", &log).in_set("late").after("draw"))
        .add_system(logger("loose", &log));
    assert_eq!(
        schedule.wave_names(),
        vec![
            vec!["integrate", "loose"],
            vec!["collide"],
            vec!["draw"],
            vec!["ui"]
        ]
    );
    let jobs = jobs();
    schedule.run(&mut World::new(), &jobs);
    assert_eq!(log.lock().unwrap().len(), 5);
}

#[test]
fn waves_are_deterministic() {
    let build = || {
        let mut schedule = Schedule::new();
        for (i, name) in ["a", "b", "c", "d", "e", "f", "g", "h"]
            .into_iter()
            .enumerate()
        {
            let mut system = System::new(name, |_| {});
            system = match i % 3 {
                0 => system.writes::<Pos>(),
                1 => system.reads::<Pos>().writes::<Vel>(),
                _ => system.reads::<Vel>(),
            };
            if i >= 4 {
                system = system.before("a");
            }
            schedule.add_system(system);
        }
        schedule.wave_names()
    };
    let first = build();
    for _ in 0..10 {
        assert_eq!(build(), first);
    }
}

#[test]
#[should_panic(expected = "system ordering cycle: a -> b -> c -> a")]
fn ordering_cycles_name_the_systems_on_them() {
    let mut schedule = Schedule::new();
    schedule
        .add_system(System::new("a", |_| {}).before("b"))
        .add_system(System::new("b", |_| {}).before("c"))
        .add_system(System::new("c", |_| {}).before("a"))
        .add_system(System::new("d", |_| {}));
    schedule.wave_names();
}

#[test]
#[should_panic(expected = "system ordering cycle")]
fn cycles_through_sets_are_found() {
    let mut schedule = Schedule::new();
    schedule.configure_set("x").before("y");
    schedule
        .add_system(System::new("in_x", |_| {}).in_set("x"))
        .add_system(System::new("in_y", |_| {}).in_set("y").before("in_x"));
    schedule.wave_names();
}

#[test]
#[should_panic(expected = "system set nesting cycle")]
fn set_nesting_cycles_are_found() {
    let mut schedule = Schedule::new();
    schedule.configure_set("x").in_set("y");
    schedule.configure_set("y").in_set("x");
    schedule.add_system(System::new("s", |_| {}).in_set("x"));
    schedule.wave_names();
}

#[test]
#[should_panic(expected = "ordering label `nowhere` names no system or set")]
fn unknown_labels_are_reported() {
    let mut schedule = Schedule::new();
    schedule.add_system(System::new("s", |_| {}).after("nowhere"));
    schedule.wave_names();
}

struct Paused(bool);

#[test]
fn run_conditions_gate_systems_and_sets() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Paused(false));
    world.insert_resource(FrameCount(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    let evaluations = Arc::new(Mutex::new(0));
    let mut schedule = Schedule::new();
    {
        let evaluations = evaluations.clone();
        schedule
            .configure_set("gameplay")
            .run_if(move |paused: Res<Paused>| {
                *evaluations.lock().unwrap() += 1;
                !paused.0
            });
    }
    schedule
        .add_system(logger("move", &log).in_set("gameplay"))
        .add_system(logger("shoot", &log).in_set("gameplay"))
        .add_system(logger("menu", &log).run_if(|paused: Res<Paused>| paused.0))
        .add_system(logger("gravity_on", &log).run_if(resource_exists::<Gravity>))
        .add_system(logger("gravity_off", &log).run_if(not(resource_exists::<Gravity>)))
        .add_system(
            (|mut frames: ResMut<FrameCount>| frames.0 += 1)
                .named("count")
                .run_if(|world: &World| world.resource::<Paused>().is_some_and(|p| !p.0)),
        );

    schedule.run(&mut world, &jobs);
    let mut ran = std::mem::take(&mut *log.lock().unwrap());
    ran.sort_unstable();
    assert_eq!(ran, ["gravity_off", "move", "shoot"]);
    assert_eq!(
        *evaluations.lock().unwrap(),
        1,
        "set conditions run once per schedule run"
    );
    assert_eq!(world.resource::<FrameCount>().unwrap().0, 1);

    world.resource_mut::<Paused>().unwrap().0 = true;
    world.insert_resource(Gravity(1.0));
    schedule.run(&mut world, &jobs);
    let mut ran = std::mem::take(&mut *log.lock().unwrap());
    ran.sort_unstable();
    assert_eq!(ran, ["gravity_on", "menu"]);
    assert_eq!(*evaluations.lock().unwrap(), 2);
    assert_eq!(world.resource::<FrameCount>().unwrap().0, 1);
}

#[test]
fn skipped_systems_see_changes_since_they_last_ran() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Paused(true));
    let e = world.spawn((Pos(0.0),));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let mut schedule = Schedule::new();
    schedule.add_system(
        System::new("watch", move |ctx| {
            let changed = ctx
                .world
                .query_filtered::<&Pos, marlin_ecs::Changed<Pos>>()
                .count();
            log.lock().unwrap().push(changed);
        })
        .reads::<Pos>()
        .run_if(|p: Res<Paused>| !p.0),
    );
    schedule.run(&mut world, &jobs);
    world.get_mut::<Pos>(e).unwrap().0 = 1.0;
    schedule.run(&mut world, &jobs);
    world.resource_mut::<Paused>().unwrap().0 = false;
    schedule.run(&mut world, &jobs);
    schedule.run(&mut world, &jobs);
    assert_eq!(*seen.lock().unwrap(), [1, 0]);
}

#[test]
fn function_systems_take_configuration_too() {
    fn early() {}
    fn late() {}
    let mut schedule = Schedule::new();
    schedule
        .add_system(late.after("early"))
        .add_system(early.in_set("start"));
    assert_eq!(schedule.wave_names(), vec![vec!["early"], vec!["late"]]);
    assert_eq!(early.into_system().name(), "early");
}
