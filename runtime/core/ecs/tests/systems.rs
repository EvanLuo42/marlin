//! Function systems: parameters, derived access, and parameter conflicts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use marlin_ecs::{
    Changed, Commands, Entity, EventReader, EventWriter, IntoSystem, Local, Query,
    RemovedComponents, Res, ResMut, Schedule, System, With, World, update_events,
};
use marlin_jobs::{Config, JobSystem};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Health(i32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Dead;

struct Time {
    dt: f32,
}

#[derive(Default)]
struct Frame(u32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Died(Entity);

fn jobs() -> JobSystem {
    JobSystem::new(Config {
        workers: 4,
        ..Config::default()
    })
    .unwrap()
}

fn movement(mut q: Query<(&mut Pos, &Vel)>, time: Res<Time>) {
    for (mut pos, vel) in &mut q {
        pos.0 += vel.0 * time.dt;
    }
}

fn gravity(mut q: Query<&mut Vel>) {
    q.for_each_mut(|mut v| v.0 -= 1.0);
}

fn count_frames(mut frame: ResMut<Frame>) {
    frame.0 += 1;
}

fn hud(q: Query<&Pos>, frame: Res<Frame>) {
    let _ = (q.iter().count(), frame.0);
}

#[test]
fn access_is_derived_from_parameters() {
    let mut schedule = Schedule::new();
    schedule
        .add_system(gravity)
        .add_system(movement)
        .add_system(count_frames)
        .add_system(hud);
    assert_eq!(
        schedule.wave_names(),
        vec![
            vec!["gravity", "count_frames"],
            vec!["movement"],
            vec!["hud"]
        ]
    );
    let system = movement.into_system();
    assert_eq!(system.name(), "movement");
    assert!(!system.access().is_read_only());
}

#[test]
fn function_systems_run_with_their_parameters() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Time { dt: 0.5 });
    world.insert_resource(Frame::default());
    let es: Vec<Entity> = (0..100)
        .map(|i| world.spawn((Pos(i as f32), Vel(2.0))))
        .collect();
    let mut schedule = Schedule::new();
    schedule
        .add_system(gravity)
        .add_system(movement)
        .add_system(count_frames);
    for _ in 0..3 {
        schedule.run(&mut world, &jobs);
    }
    // Velocity 1, 0, -1 applied for half a second each.
    for (i, &e) in es.iter().enumerate() {
        assert_eq!(world.get::<Pos>(e), Some(&Pos(i as f32)));
        assert_eq!(world.get::<Vel>(e), Some(&Vel(-1.0)));
    }
    assert_eq!(world.resource::<Frame>().unwrap().0, 3);
}

#[test]
fn commands_and_locals_belong_to_the_system() {
    let jobs = jobs();
    let mut world = World::new();
    let spawned = Arc::new(Mutex::new(Vec::new()));
    let log = spawned.clone();
    let spawner = move |mut commands: Commands, mut runs: Local<u32>| {
        *runs += 1;
        let parent = commands.spawn((Health(*runs as i32),));
        // An id from this run's buffer, used in the same buffer.
        let child = commands.spawn((Pos(0.0),));
        commands.insert(child, Health(-1));
        log.lock().unwrap().push((parent, child));
    };
    let mut schedule = Schedule::new();
    schedule.add_system(spawner);
    schedule.run(&mut world, &jobs);
    schedule.run(&mut world, &jobs);
    let spawned = spawned.lock().unwrap();
    assert_eq!(spawned.len(), 2);
    assert_eq!(world.get::<Health>(spawned[0].0), Some(&Health(1)));
    assert_eq!(world.get::<Health>(spawned[1].0), Some(&Health(2)));
    assert_eq!(world.get::<Health>(spawned[1].1), Some(&Health(-1)));
    assert_eq!(world.entity_count(), 4);
}

#[test]
fn queries_get_filter_and_count() {
    let jobs = jobs();
    let mut world = World::new();
    let a = world.spawn((Pos(1.0), Health(10)));
    let b = world.spawn((Pos(2.0),));
    let seen = Arc::new(Mutex::new(None));
    let out = seen.clone();
    let check = move |q: Query<(Entity, &Pos), With<Health>>, all: Query<&Pos>| {
        *out.lock().unwrap() = Some((
            q.count(),
            q.get(a).map(|(_, p)| p.0),
            q.get(b).is_none(),
            q.contains(a),
            q.single().map(|(e, _)| e),
            all.count(),
            all.single().is_none(),
        ));
    };
    let mut schedule = Schedule::new();
    schedule.add_system(check);
    schedule.run(&mut world, &jobs);
    assert_eq!(
        *seen.lock().unwrap(),
        Some((1, Some(1.0), true, true, Some(a), 2, true))
    );
}

#[test]
fn change_detection_and_removals_per_system() {
    let jobs = jobs();
    let mut world = World::new();
    let es: Vec<Entity> = (0..10).map(|i| world.spawn((Health(i),))).collect();
    let changed = Arc::new(AtomicUsize::new(0));
    let removed = Arc::new(AtomicUsize::new(0));
    let (c, r) = (changed.clone(), removed.clone());
    let watcher = move |q: Query<Entity, Changed<Health>>, gone: RemovedComponents<Health>| {
        c.store(q.iter().count(), Ordering::Relaxed);
        r.store(gone.iter().count(), Ordering::Relaxed);
    };
    let mut schedule = Schedule::new();
    schedule.add_system(watcher);
    schedule.run(&mut world, &jobs);
    assert_eq!(changed.load(Ordering::Relaxed), 10);
    world.get_mut::<Health>(es[3]).unwrap().0 = 99;
    world.despawn(es[4]);
    schedule.run(&mut world, &jobs);
    assert_eq!(changed.load(Ordering::Relaxed), 1);
    assert_eq!(removed.load(Ordering::Relaxed), 1);
    schedule.run(&mut world, &jobs);
    assert_eq!(changed.load(Ordering::Relaxed), 0);
    assert_eq!(removed.load(Ordering::Relaxed), 0);
}

#[test]
fn events_flow_between_systems() {
    let jobs = jobs();
    let mut world = World::new();
    world.add_event::<Died>();
    let es: Vec<Entity> = (0..6).map(|i| world.spawn((Health(i - 2),))).collect();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let reaper = |q: Query<(Entity, &Health), bevy_like::Alive>, mut died: EventWriter<Died>| {
        for (e, h) in &q {
            if h.0 <= 0 {
                died.send(Died(e));
            }
        }
    };
    let mark = |mut events: EventReader<Died>, mut commands: Commands| {
        for &Died(e) in events.read() {
            commands.insert(e, Dead);
        }
    };
    let listen = move |mut events: EventReader<Died>| {
        log.lock().unwrap().extend(events.read().copied());
    };
    let mut schedule = Schedule::new();
    schedule
        .add_system(update_events::<Died>)
        .add_system(reaper.named("reaper").after("update_events"))
        .add_system(mark.after("reaper"))
        .add_system(listen.after("reaper"));
    schedule.run(&mut world, &jobs);
    schedule.run(&mut world, &jobs);
    // Each death reported once, though the event lives for two updates.
    let seen = seen.lock().unwrap();
    assert_eq!(*seen, [Died(es[0]), Died(es[1]), Died(es[2])]);
    assert_eq!(world.query_filtered::<Entity, With<Dead>>().count(), 3);
}

/// A filter alias, as a game would write one.
mod bevy_like {
    pub type Alive = marlin_ecs::Without<super::Dead>;
}

#[test]
fn whole_world_readers_and_exclusive_functions() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Frame::default());
    world.spawn((Pos(1.0),));
    let total = Arc::new(Mutex::new(0.0));
    let out = total.clone();
    let reader = move |world: &World| {
        *out.lock().unwrap() = world.query_ref::<&Pos>().map(|p| p.0).sum::<f32>();
    };
    let exclusive = |world: &mut World| {
        world.spawn((Pos(10.0),));
    };
    let mut schedule = Schedule::new();
    schedule
        .add_system(count_frames)
        .add_system(reader)
        .add_system(exclusive)
        .add_system(gravity);
    // `&World` reads everything, so it waits for the writer; the exclusive system runs alone.
    assert_eq!(
        schedule.wave_names(),
        vec![
            vec!["count_frames"],
            vec!["{{closure}}"],
            vec!["{{closure}}"],
            vec!["gravity"]
        ]
    );
    schedule.run(&mut world, &jobs);
    assert_eq!(*total.lock().unwrap(), 1.0);
    assert_eq!(world.query::<&Pos>().count(), 2);
}

#[test]
fn par_for_each_through_a_query_parameter() {
    let jobs = jobs();
    let mut world = World::new();
    world.spawn_batch((0..20_000).map(|i| (Pos(0.0), Vel(i as f32))));
    let mut schedule = Schedule::new();
    schedule.add_system(|mut q: Query<(&mut Pos, &Vel)>| {
        q.par_for_each(512, |(mut p, v)| p.0 = v.0 * 2.0);
    });
    schedule.run(&mut world, &jobs);
    assert!(
        world
            .query_ref::<(&Pos, &Vel)>()
            .all(|(p, v)| p.0 == v.0 * 2.0)
    );
}

#[test]
fn closure_systems_keep_explicit_declarations() {
    let jobs = jobs();
    let mut world = World::new();
    world.spawn((Pos(1.0),));
    let mut schedule = Schedule::new();
    schedule.add_system(
        System::new("explicit", |ctx| {
            for mut p in ctx.world.query_mut::<&mut Pos>() {
                p.0 += 1.0;
            }
        })
        .writes::<Pos>(),
    );
    schedule.add_system(|q: Query<&Pos>| assert_eq!(q.single().unwrap().0, 2.0));
    assert_eq!(
        schedule.wave_names().len(),
        2,
        "the reader waits for the writer"
    );
    schedule.run(&mut world, &jobs);
}

#[test]
#[should_panic(expected = "system `clash` has conflicting parameters")]
fn conflicting_queries_are_rejected_when_built() {
    fn clash(_: Query<&mut Pos>, _: Query<&Pos>) {}
    clash.into_system();
}

#[test]
#[should_panic(expected = "conflicting parameters")]
fn conflicting_resources_are_rejected_when_built() {
    let mut schedule = Schedule::new();
    schedule.add_system(|_: Res<Frame>, _: ResMut<Frame>| {});
}

#[test]
#[should_panic(expected = "conflicting parameters")]
fn whole_world_readers_cannot_also_write() {
    let mut schedule = Schedule::new();
    schedule.add_system(|_: &World, _: Query<&mut Pos>| {});
}

#[test]
#[should_panic(expected = "mutably")]
fn a_self_aliasing_query_parameter_is_rejected() {
    let mut schedule = Schedule::new();
    schedule.add_system(|_: Query<(&mut Pos, &Pos)>| {});
}

#[test]
#[should_panic(expected = "needs resource")]
fn missing_resources_panic_with_the_system_name() {
    let jobs = jobs();
    let mut world = World::new();
    let mut schedule = Schedule::new();
    schedule.add_system(movement);
    schedule.run(&mut world, &jobs);
}

#[test]
fn optional_resources_and_many_parameters() {
    let jobs = jobs();
    let mut world = World::new();
    world.insert_resource(Frame(7));
    let out = Arc::new(Mutex::new(None));
    let o = out.clone();
    #[allow(clippy::too_many_arguments)]
    let many = move |a: Option<Res<Frame>>,
                     b: Option<Res<Time>>,
                     _c: Query<&Pos>,
                     _d: Query<&Vel>,
                     _e: Local<u8>,
                     _f: Commands,
                     _g: Query<&Health>,
                     _h: Query<Entity>,
                     _i: Local<u16>,
                     _j: Query<&Dead>,
                     _k: RemovedComponents<Pos>,
                     _l: Local<u32>| {
        *o.lock().unwrap() = Some((a.map(|f| f.0), b.is_none()));
    };
    let mut schedule = Schedule::new();
    schedule.add_system(many);
    schedule.run(&mut world, &jobs);
    assert_eq!(*out.lock().unwrap(), Some((Some(7), true)));
}
