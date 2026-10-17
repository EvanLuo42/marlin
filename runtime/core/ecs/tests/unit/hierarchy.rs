//! Parent/child hierarchy: both sides kept consistent by the operations and the hooks.

use marlin_ecs::hierarchy::descendants;
use marlin_ecs::{Children, CommandQueue, Commands, Entity, Parent, Query, Schedule, World};

use crate::support::jobs;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Name(&'static str);

/// Checks every `Parent` is listed by its parent and every listed child points back.
fn assert_consistent(world: &mut World) {
    let pairs: Vec<(Entity, Entity)> = world
        .query::<(Entity, &Parent)>()
        .map(|(e, p)| (e, p.get()))
        .collect();
    for (child, parent) in pairs {
        assert!(world.is_alive(parent), "{child:?} has a dead parent");
        assert!(
            world.children(parent).contains(&child),
            "{parent:?} lost {child:?}"
        );
    }
    let lists: Vec<(Entity, Vec<Entity>)> = world
        .query::<(Entity, &Children)>()
        .map(|(e, c)| (e, c.to_vec()))
        .collect();
    for (parent, children) in lists {
        assert!(!children.is_empty(), "empty `Children` on {parent:?}");
        for child in children {
            assert_eq!(world.parent(child), Some(parent));
        }
    }
}

fn tree(world: &mut World) -> [Entity; 6] {
    // root -> a -> (a1, a2), root -> b -> b1
    let [root, a, a1, a2, b, b1] =
        ["root", "a", "a1", "a2", "b", "b1"].map(|n| world.spawn((Name(n),)));
    world.set_parent(a, root);
    world.set_parent(b, root);
    world.set_parent(a1, a);
    world.set_parent(a2, a);
    world.set_parent(b1, b);
    [root, a, a1, a2, b, b1]
}

#[test]
fn set_parent_links_both_sides_and_iterates() {
    let mut world = World::new();
    let [root, a, a1, a2, b, b1] = tree(&mut world);
    assert_eq!(world.children(root), [a, b]);
    assert_eq!(world.parent(a1), Some(a));
    assert_eq!(
        world.descendants(root).collect::<Vec<_>>(),
        [a, a1, a2, b, b1]
    );
    assert_eq!(world.ancestors(a2).collect::<Vec<_>>(), [a, root]);
    assert_eq!(world.root(b1), root);
    assert_eq!(world.root(root), root);
    assert_consistent(&mut world);

    // Reparenting moves the child between lists, keeping the order of the others.
    assert!(world.set_parent(a1, b));
    assert_eq!(world.children(a), [a2]);
    assert_eq!(world.children(b), [b1, a1]);
    assert!(
        world.set_parent(a1, b),
        "setting the same parent again is a no-op"
    );
    assert_eq!(world.children(b), [b1, a1]);

    assert!(world.remove_parent(a2));
    assert!(!world.remove_parent(a2));
    assert!(!world.has::<Children>(a), "an empty list goes away");
    assert_eq!(world.parent(a2), None);
    assert_consistent(&mut world);
}

#[test]
fn despawning_either_side_keeps_the_other_consistent() {
    let mut world = World::new();
    let [root, a, a1, a2, b, b1] = tree(&mut world);
    // A plain despawn of a child detaches it from its parent.
    world.despawn(a1);
    assert_eq!(world.children(a), [a2]);
    // A plain despawn of a parent orphans its children.
    world.despawn(b);
    assert_eq!(world.parent(b1), None);
    assert_eq!(world.children(root), [a]);
    assert_consistent(&mut world);

    // Recursive despawn takes the subtree and detaches its root.
    world.set_parent(b1, a2);
    assert!(world.despawn_recursive(a));
    for e in [a, a2, b1] {
        assert!(!world.is_alive(e));
    }
    assert!(world.is_alive(root));
    assert!(world.children(root).is_empty());
    assert_eq!(world.entity_count(), 1);
    assert_consistent(&mut world);
}

#[test]
fn cloned_children_join_the_same_parent() {
    let mut world = World::new();
    world.register_cloneable::<Name>();
    let [root, _, _, _, b, b1] = tree(&mut world);
    let copy = world
        .clone_entity(b1)
        .expect("Name and Parent are cloneable");
    assert_eq!(world.get::<Name>(copy), Some(&Name("b1")));
    assert_eq!(world.parent(copy), Some(b));
    assert_eq!(world.children(b), [b1, copy]);
    assert!(
        world.clone_entity(root).is_none(),
        "`Children` is not cloneable"
    );
    assert_consistent(&mut world);
}

#[test]
fn clones_into_other_worlds_leave_the_hierarchy_behind() {
    let mut world = World::new();
    world.register_cloneable::<Name>();
    let [root, a, ..] = tree(&mut world);
    let mut other = World::new();
    let other_root = other.spawn((Name("other"),));
    let copy = world.clone_entity_to(a, &mut other).unwrap();
    assert_eq!(other.get::<Name>(copy), Some(&Name("a")));
    assert_eq!(other.parent(copy), None);
    assert!(other.children(copy).is_empty());
    assert!(other.children(other_root).is_empty());
    assert!(world.clone_entity_to(root, &mut other).is_some());
    assert_consistent(&mut other);
}

#[test]
#[should_panic(expected = "would make a cycle")]
fn cycles_are_rejected() {
    let mut world = World::new();
    let [root, _, a1, ..] = tree(&mut world);
    world.set_parent(root, a1);
}

#[test]
fn commands_build_hierarchies_and_systems_walk_them() {
    let jobs = jobs(2);
    let mut world = World::new();
    let mut queue = CommandQueue::new();
    let (root, kids) = {
        let mut commands = Commands::new(&mut queue, &world);
        let root = commands.spawn((Name("root"),));
        let kids: Vec<Entity> = (0..3)
            .map(|_| commands.spawn_child(root, (Name("kid"),)))
            .collect();
        commands.spawn_child(kids[1], (Name("grandkid"),));
        (root, kids)
    };
    world.apply(&mut queue);
    assert_eq!(world.children(root), kids);
    assert_consistent(&mut world);

    let mut schedule = Schedule::new();
    schedule.add_system(
        move |children: Query<&Children>, names: Query<&Name>, mut commands: Commands| {
            let below: Vec<&str> =
                descendants(root, |e| children.get(e).map_or(&[][..], |c| c.as_slice()))
                    .map(|e| names.get(e).unwrap().0)
                    .collect();
            assert_eq!(below, ["kid", "kid", "grandkid", "kid"]);
            commands.despawn_recursive(root);
        },
    );
    schedule.run(&mut world, &jobs);
    assert_eq!(world.entity_count(), 0);
}
