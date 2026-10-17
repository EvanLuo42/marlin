//! Commands: reserved ids, ordering, and queues bound to their world.

use marlin_ecs::{CommandQueue, Commands, Entity, World};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Target(Entity);

#[test]
fn spawn_returns_ids_later_commands_can_use() {
    let mut world = World::new();
    let mut queue = CommandQueue::new();
    let (a, b) = {
        let mut commands = Commands::new(&mut queue, &world);
        let a = commands.spawn((Pos(1.0),));
        let b = commands.spawn((Pos(2.0), Target(a)));
        // Refer to both before either exists.
        commands.insert(a, Vel(10.0));
        commands.insert(b, Vel(20.0));
        commands.insert(a, Target(b));
        (a, b)
    };
    assert_ne!(a, b);
    assert!(!world.is_alive(a), "not yet applied");
    world.apply(&mut queue);
    assert!(queue.is_empty());
    assert_eq!(world.get::<Pos>(a), Some(&Pos(1.0)));
    assert_eq!(world.get::<Vel>(a), Some(&Vel(10.0)));
    assert_eq!(world.get::<Target>(a), Some(&Target(b)));
    assert_eq!(world.get::<Target>(b), Some(&Target(a)));
    assert_eq!(world.get::<Vel>(b), Some(&Vel(20.0)));
    assert_eq!(world.entity_count(), 2);
}

#[test]
fn reserved_ids_reuse_freed_slots_without_aliasing() {
    let mut world = World::new();
    let olds: Vec<Entity> = (0..5).map(|i| world.spawn((Pos(i as f32),))).collect();
    for &e in &olds[..3] {
        world.despawn(e);
    }
    let mut queue = CommandQueue::new();
    let reserved: Vec<Entity> = {
        let mut commands = Commands::new(&mut queue, &world);
        (0..6)
            .map(|i| commands.spawn((Pos(100.0 + i as f32),)))
            .collect()
    };
    // Three reuse freed slots (new generations), three are fresh.
    let mut indices: Vec<u32> = reserved.iter().map(|e| e.index()).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices.len(), 6, "distinct ids");
    for old in &olds[..3] {
        assert!(!reserved.contains(old));
    }
    // A direct spawn before applying must not take a reserved slot.
    let direct = world.spawn((Pos(-1.0),));
    assert!(!reserved.contains(&direct));
    world.apply(&mut queue);
    for (i, &e) in reserved.iter().enumerate() {
        assert_eq!(world.get::<Pos>(e), Some(&Pos(100.0 + i as f32)));
    }
    assert_eq!(world.get::<Pos>(direct), Some(&Pos(-1.0)));
    assert_eq!(world.entity_count(), 2 + 6 + 1);
    for &e in &olds[..3] {
        assert!(!world.is_alive(e));
    }
}

#[test]
fn reserved_entities_without_bundles_are_empty_entities() {
    let mut world = World::new();
    let e = world.reserve_entity();
    assert!(!world.is_alive(e));
    world.flush();
    assert!(world.is_alive(e));
    assert_eq!(world.entity_components(e), Some(&[][..]));
    world.insert(e, Pos(3.0));
    assert_eq!(world.get::<Pos>(e), Some(&Pos(3.0)));
}

#[test]
fn commands_apply_in_insertion_order() {
    let mut world = World::new();
    let e = world.spawn((Pos(0.0),));
    let mut queue = CommandQueue::new();
    {
        let mut commands = Commands::new(&mut queue, &world);
        commands.insert(e, Pos(1.0));
        commands.remove::<Pos>(e);
        commands.insert(e, Pos(2.0));
        commands.add(move |w| w.get_mut::<Pos>(e).unwrap().0 *= 10.0);
        commands.insert_resource(7u32);
        commands.spawn_batch((0..4).map(|i| (Vel(i as f32),)));
    }
    world.apply(&mut queue);
    assert_eq!(world.get::<Pos>(e), Some(&Pos(20.0)));
    assert_eq!(world.resource::<u32>(), Some(&7));
    assert_eq!(world.query::<&Vel>().count(), 4);
}

#[test]
#[should_panic(expected = "world other than")]
fn queues_belong_to_the_world_that_reserved_their_ids() {
    let world = World::new();
    let mut other = World::new();
    let mut queue = CommandQueue::new();
    Commands::new(&mut queue, &world).spawn((Pos(0.0),));
    other.apply(&mut queue);
}
