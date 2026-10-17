//! Component hooks: when they run, what they see, and their deferred commands.

use std::sync::Mutex;

use marlin_ecs::{CommandQueue, Commands, Entity, HookContext, StorageType, World};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Health(i32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Tag;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Echo(u32);

/// Hook calls in order: (hook, entity index, what the hook read).
static LOG: Mutex<Vec<(&str, u32, Option<i32>)>> = Mutex::new(Vec::new());

fn record(which: &'static str, ctx: &HookContext<'_, '_>) {
    let value = ctx.world.get::<Health>(ctx.entity).map(|h| h.0);
    LOG.lock().unwrap().push((which, ctx.entity.index(), value));
}

fn on_add(ctx: &mut HookContext<'_, '_>) {
    record("add", ctx);
}

fn on_insert(ctx: &mut HookContext<'_, '_>) {
    record("insert", ctx);
}

fn on_replace(ctx: &mut HookContext<'_, '_>) {
    record("replace", ctx);
}

fn on_remove(ctx: &mut HookContext<'_, '_>) {
    record("remove", ctx);
}

fn take_log() -> Vec<(&'static str, u32, Option<i32>)> {
    std::mem::take(&mut *LOG.lock().unwrap())
}

/// All hook tests share the log, so they run as one test.
#[test]
fn hooks_run_in_the_documented_order() {
    for storage in [StorageType::Table, StorageType::SparseSet] {
        take_log();
        let mut world = World::new();
        world.register_component_with::<Health>(storage);
        world
            .component_hooks::<Health>()
            .on_add(on_add)
            .on_insert(on_insert)
            .on_replace(on_replace)
            .on_remove(on_remove);

        // Spawn: add, then insert, both after the value is in place.
        let e = world.spawn((Pos(0.0), Health(1)));
        let i = e.index();
        assert_eq!(take_log(), [("add", i, Some(1)), ("insert", i, Some(1))]);

        // Replace: the old value is visible to `replace`, the new one to `insert`; no `add`.
        world.insert(e, Health(2));
        assert_eq!(
            take_log(),
            [("replace", i, Some(1)), ("insert", i, Some(2))]
        );

        // Unrelated inserts trigger nothing.
        world.insert(e, Tag);
        assert!(take_log().is_empty());

        // Remove: both hooks see the value before it goes.
        assert_eq!(world.remove::<Health>(e), Some(Health(2)));
        assert_eq!(
            take_log(),
            [("replace", i, Some(2)), ("remove", i, Some(2))]
        );
        assert_eq!(world.remove::<Health>(e), None);
        assert!(take_log().is_empty());

        // Despawn removes with hooks; batch spawns hook every entity.
        world.insert(e, Health(3));
        take_log();
        world.despawn(e);
        assert_eq!(
            take_log(),
            [("replace", i, Some(3)), ("remove", i, Some(3))]
        );
        let es = world.spawn_batch((0..3).map(|n| (Health(10 + n),)));
        let log = take_log();
        assert_eq!(log.len(), 6);
        for (k, e) in es.iter().enumerate() {
            assert!(log.contains(&("add", e.index(), Some(10 + k as i32))));
        }

        // Applied commands trigger hooks like direct calls.
        let mut queue = CommandQueue::new();
        let spawned = Commands::new(&mut queue, &world).spawn((Health(7),));
        world.apply(&mut queue);
        assert_eq!(
            take_log(),
            [
                ("add", spawned.index(), Some(7)),
                ("insert", spawned.index(), Some(7))
            ]
        );
    }
}

/// `Echo(n)` spawns `Echo(n - 1)` through its hook's commands until zero.
fn echo(ctx: &mut HookContext<'_, '_>) {
    let n = ctx.world.get::<Echo>(ctx.entity).unwrap().0;
    // The world is unchanged while the hook runs: its commands wait.
    assert_eq!(ctx.world.get::<Echo>(ctx.entity), Some(&Echo(n)));
    if n > 0 {
        let parent = ctx.entity;
        let child = ctx.commands.spawn((Echo(n - 1),));
        ctx.commands.add(move |world| {
            world.insert(parent, Pos(child.index() as f32));
        });
    }
}

#[test]
fn hook_commands_apply_when_the_triggering_call_returns() {
    let mut world = World::new();
    world.component_hooks::<Echo>().on_add(echo);
    let root = world.spawn((Echo(5),));
    // The whole chain ran before `spawn` returned.
    assert_eq!(world.query::<&Echo>().count(), 6);
    assert!(world.has::<Pos>(root));
    let mut values: Vec<u32> = world.query::<&Echo>().map(|e| e.0).collect();
    values.sort_unstable();
    assert_eq!(values, [0, 1, 2, 3, 4, 5]);
    // Ones with children got a `Pos` naming them; the last did not.
    assert_eq!(world.query::<(&Echo, &Pos)>().count(), 5);
    let last: Vec<Entity> = world
        .query_filtered::<Entity, marlin_ecs::Without<Pos>>()
        .collect();
    assert_eq!(last.len(), 1);
    assert_eq!(world.get::<Echo>(last[0]), Some(&Echo(0)));
}

/// Spawns a `Tag` entity naming the despawned one in its `Echo`.
fn spawn_on_remove(ctx: &mut HookContext<'_, '_>) {
    let index = ctx.entity.index();
    ctx.commands.spawn((Tag, Echo(index)));
    ctx.commands.spawn_empty();
}

#[test]
fn despawn_keeps_ids_hooks_reserve() {
    let mut world = World::new();
    world.component_hooks::<Health>().on_remove(spawn_on_remove);
    // A freed id first, so the hook's reservations reuse it as well as taking fresh ones.
    let freed = world.spawn((Pos(0.0),));
    world.despawn(freed);
    let a = world.spawn((Health(1),));
    let b = world.spawn((Health(2),));
    let others = world.spawn((Pos(1.0),));
    assert_eq!(world.entity_count(), 3);

    for e in [a, b] {
        assert!(world.despawn(e));
        assert!(!world.is_alive(e));
    }
    // Each despawn left a tagged entity and an empty one, all distinct and alive.
    assert_eq!(world.entity_count(), 1 + 2 * 2);
    let mut tagged: Vec<(Entity, u32)> = world
        .query::<(Entity, &Echo)>()
        .map(|(e, n)| (e, n.0))
        .collect();
    tagged.sort_unstable_by_key(|&(_, n)| n);
    assert_eq!(
        tagged.iter().map(|&(_, n)| n).collect::<Vec<_>>(),
        [a.index(), b.index()]
    );
    for &(e, _) in &tagged {
        assert!(world.has::<Tag>(e));
        assert_ne!(e, others);
    }
    // Ids spawned afterwards do not alias the reserved ones.
    let next = world.spawn((Pos(2.0),));
    assert!(tagged.iter().all(|&(e, _)| e.index() != next.index()));
    assert_eq!(world.entity_count(), 6);
}

#[test]
#[should_panic(expected = "already has an `on_add` hook")]
fn one_hook_per_slot() {
    let mut world = World::new();
    world.component_hooks::<Echo>().on_add(echo);
    world.component_hooks::<Echo>().on_add(echo);
}
