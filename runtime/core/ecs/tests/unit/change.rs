//! Change detection: added and changed ticks, `Mut` and `Ref`, the `Changed` / `Added` filters,
//! removals, and each system seeing what changed since its own previous run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use marlin_ecs::{Added, Changed, Entity, Ref, Schedule, System, Tick, With, World};

use crate::support::jobs;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Tag;

fn changed_pos(world: &mut World) -> Vec<Entity> {
    let mut v: Vec<Entity> = world.query_filtered::<Entity, Changed<Pos>>().collect();
    v.sort_by_key(|e| e.index());
    v
}

fn added_pos(world: &mut World) -> Vec<Entity> {
    let mut v: Vec<Entity> = world.query_filtered::<Entity, Added<Pos>>().collect();
    v.sort_by_key(|e| e.index());
    v
}

#[test]
fn spawns_are_added_until_cleared() {
    let mut world = World::new();
    let a = world.spawn((Pos(0.0),));
    let b = world.spawn((Pos(1.0), Vel(1.0)));
    assert_eq!(added_pos(&mut world), [a, b]);
    assert_eq!(changed_pos(&mut world), [a, b]);
    world.clear_trackers();
    assert!(added_pos(&mut world).is_empty());
    assert!(changed_pos(&mut world).is_empty());
    let c = world.spawn((Pos(2.0),));
    assert_eq!(added_pos(&mut world), [c]);
}

#[test]
fn writes_through_mut_mark_changes_and_reads_do_not() {
    let mut world = World::new();
    let es: Vec<Entity> = (0..10).map(|i| world.spawn((Pos(i as f32),))).collect();
    world.clear_trackers();

    // Reading through the `Mut` is not a change.
    let sum: f32 = world.query::<&mut Pos>().map(|p| p.0).sum();
    assert_eq!(sum, 45.0);
    assert!(changed_pos(&mut world).is_empty());

    // Writing through it is; bypassing is not.
    for (i, mut p) in world.query::<&mut Pos>().enumerate() {
        match i % 3 {
            0 => p.0 += 1.0,
            1 => p.bypass_change_detection().0 += 1.0,
            _ => p.set_if_neq(Pos(p.0)),
        }
    }
    let expected: Vec<Entity> = es.iter().copied().step_by(3).collect();
    assert_eq!(changed_pos(&mut world), expected);
    assert!(added_pos(&mut world).is_empty());

    // `Mut` reports its own state.
    for p in world.query::<&mut Pos>() {
        assert!(!p.is_added());
        assert_eq!(p.is_changed(), p.0 as usize % 3 == 1, "{p:?}");
    }
    // As does `Ref`.
    for (e, p) in world.query_ref::<(Entity, Ref<Pos>)>() {
        assert_eq!(p.is_changed(), expected.contains(&e));
        assert!(!p.is_added());
    }
}

#[test]
fn get_mut_and_insert_mark_changes() {
    let mut world = World::new();
    let a = world.spawn((Pos(0.0),));
    let b = world.spawn((Pos(1.0),));
    world.clear_trackers();
    world.get_mut::<Pos>(a).unwrap().0 = 5.0;
    assert_eq!(world.get::<Pos>(a), Some(&Pos(5.0)));
    assert_eq!(changed_pos(&mut world), [a]);
    // Replacing a component changes it; it was not added.
    world.insert(b, Pos(9.0));
    assert_eq!(changed_pos(&mut world), [a, b]);
    assert!(added_pos(&mut world).is_empty());
    let ticks = world.component_ticks::<Pos>(b).unwrap();
    assert!(ticks.changed.get() > ticks.added.get());
}

#[test]
fn moving_between_archetypes_keeps_ticks() {
    let mut world = World::new();
    let a = world.spawn((Pos(0.0),));
    world.clear_trackers();
    // Adding `Vel` moves `a` to another archetype: its `Pos` is not new or changed.
    world.insert(a, Vel(1.0));
    assert!(changed_pos(&mut world).is_empty());
    let added: Vec<Entity> = world.query_filtered::<Entity, Added<Vel>>().collect();
    assert_eq!(added, [a]);
    world.remove::<Vel>(a);
    assert!(changed_pos(&mut world).is_empty());
}

#[test]
fn changed_filters_combine_with_writes_and_other_filters() {
    let mut world = World::new();
    let tagged = world.spawn((Pos(0.0), Tag));
    let plain = world.spawn((Pos(0.0),));
    world.clear_trackers();
    for mut p in world.query::<&mut Pos>() {
        p.0 = 1.0;
    }
    // A query may write what its filter tests.
    let mut count = 0;
    for mut p in world.query_filtered::<&mut Pos, Changed<Pos>>() {
        p.0 += 1.0;
        count += 1;
    }
    assert_eq!(count, 2);
    let tagged_changed: Vec<Entity> = world
        .query_filtered::<Entity, (Changed<Pos>, With<Tag>)>()
        .collect();
    assert_eq!(tagged_changed, [tagged]);
    assert!(world.is_alive(plain));
}

#[test]
fn par_for_each_filters_rows() {
    let jobs = jobs(4);
    let mut world = World::new();
    let es: Vec<Entity> = (0..5000).map(|i| world.spawn((Pos(i as f32),))).collect();
    world.clear_trackers();
    for &e in es.iter().step_by(7) {
        world.get_mut::<Pos>(e).unwrap().0 = -1.0;
    }
    let seen = AtomicUsize::new(0);
    world.par_for_each_filtered::<&Pos, Changed<Pos>, _>(&jobs, 64, |p| {
        assert_eq!(p.0, -1.0);
        seen.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(seen.load(Ordering::Relaxed), es.len().div_ceil(7));
}

#[test]
fn removals_are_listed_for_two_periods() {
    let mut world = World::new();
    let a = world.spawn((Pos(0.0), Vel(0.0)));
    let b = world.spawn((Pos(1.0),));
    let c = world.spawn((Pos(2.0),));
    world.remove::<Pos>(a);
    world.despawn(b);
    let mut removed: Vec<Entity> = world.removed::<Pos>().collect();
    removed.sort_by_key(|e| e.index());
    assert_eq!(removed, [a, b]);
    assert_eq!(world.removed::<Vel>().count(), 0);
    world.clear_trackers();
    // Not reported any more through the world, but kept for readers that look once a period.
    assert_eq!(world.removed::<Pos>().count(), 0);
    world.despawn(c);
    assert_eq!(world.removed::<Pos>().collect::<Vec<_>>(), [c]);
    world.clear_trackers();
    world.clear_trackers();
    assert_eq!(world.removed::<Pos>().count(), 0);
    assert_eq!(
        world
            .removed_since::<Pos>(Tick::new(0), world.change_tick())
            .count(),
        0,
        "pruned after two periods"
    );
}

/// Each system sees the changes made since its own previous run, by any system.
#[test]
fn systems_see_changes_since_their_last_run() {
    let jobs = jobs(4);
    let mut world = World::new();
    let es: Vec<Entity> = (0..20)
        .map(|i| world.spawn((Pos(i as f32), Vel(0.0))))
        .collect();
    let writes: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let seen: Arc<Mutex<Vec<Vec<Entity>>>> = Arc::new(Mutex::new(Vec::new()));
    let removed_seen: Arc<Mutex<Vec<Vec<Entity>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut schedule = Schedule::new();
    {
        let writes = writes.clone();
        schedule.add_system(
            System::new("writer", move |ctx| {
                let which = std::mem::take(&mut *writes.lock().unwrap());
                for (i, mut p) in ctx.world.query_mut::<&mut Pos>().enumerate() {
                    if which.contains(&i) {
                        p.0 += 100.0;
                    }
                }
            })
            .writes::<Pos>(),
        );
    }
    {
        let seen = seen.clone();
        let removed_seen = removed_seen.clone();
        schedule.add_system(
            System::new("reader", move |ctx| {
                let mut changed: Vec<Entity> =
                    ctx.world.query_filtered::<Entity, Changed<Pos>>().collect();
                changed.sort_by_key(|e| e.index());
                seen.lock().unwrap().push(changed);
                let mut removed: Vec<Entity> = ctx.world.removed::<Pos>().collect();
                removed.sort_by_key(|e| e.index());
                removed_seen.lock().unwrap().push(removed);
            })
            .reads::<Pos>(),
        );
    }
    // A system that writes but never sees its own writes as changes on its next run.
    let self_seen = Arc::new(AtomicUsize::new(usize::MAX));
    {
        let self_seen = self_seen.clone();
        let mut runs = 0;
        schedule.add_system(
            System::new("self", move |ctx| {
                runs += 1;
                let n = ctx
                    .world
                    .query_mut_filtered::<&mut Vel, Changed<Vel>>()
                    .count();
                if runs > 1 {
                    self_seen.store(n, Ordering::Relaxed);
                }
                for mut v in ctx.world.query_mut::<&mut Vel>() {
                    v.0 += 1.0;
                }
            })
            .writes::<Vel>(),
        );
    }

    // Run 1: everything is new to every system.
    schedule.run(&mut world, &jobs);
    // Run 2: entities in rows 3 and 5 are written.
    *writes.lock().unwrap() = vec![3, 5];
    schedule.run(&mut world, &jobs);
    // Run 3: nothing written; one entity despawned in between.
    world.despawn(es[7]);
    schedule.run(&mut world, &jobs);

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].len(), 20);
    assert_eq!(seen[1].len(), 2);
    assert!(seen[2].is_empty());
    let removed_seen = removed_seen.lock().unwrap();
    assert!(removed_seen[0].is_empty() && removed_seen[1].is_empty());
    assert_eq!(removed_seen[2], [es[7]]);
    assert_eq!(self_seen.load(Ordering::Relaxed), 0);
}

#[test]
#[should_panic(expected = "accesses `unit::change::Pos` without declaring it")]
fn changed_filters_need_declared_reads() {
    let jobs = jobs(4);
    let mut world = World::new();
    world.spawn((Pos(0.0), Vel(0.0)));
    let mut schedule = Schedule::new();
    schedule.add_system(
        System::new("sneaky", |ctx| {
            ctx.world.query_filtered::<&Vel, Changed<Pos>>().count();
        })
        .reads::<Vel>(),
    );
    schedule.run(&mut world, &jobs);
}

#[test]
fn ticks_compare_across_wraparound() {
    let near_end = Tick::new(u32::MAX - 5);
    let last_run = Tick::new(u32::MAX - 10);
    let now = Tick::new(3);
    assert!(near_end.is_newer_than(last_run, now));
    assert!(!last_run.is_newer_than(near_end, now));
    assert!(Tick::new(2).is_newer_than(near_end, now));
    // A tick equal to `last_run` was seen already.
    assert!(!last_run.is_newer_than(last_run, now));
}
