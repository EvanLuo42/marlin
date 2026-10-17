//! Cached queries: archetypes created later, components registered later, lookups by entity.

use marlin_ecs::{Changed, Entity, QueryState, With, Without, World};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Frozen;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Late;

#[test]
fn picks_up_archetypes_created_after_it() {
    let mut world = World::new();
    world.spawn((Pos(1.0), Vel(1.0)));
    let mut state = world.query_state::<(&mut Pos, &Vel), ()>();
    assert_eq!(state.iter_mut(&mut world).count(), 1);
    let matched_before = state.matched_archetypes().len();

    world.spawn((Pos(2.0), Vel(1.0), Frozen));
    world.spawn((Pos(3.0),));
    for (mut p, v) in state.iter_mut(&mut world) {
        p.0 += v.0;
    }
    assert_eq!(state.matched_archetypes().len(), matched_before + 1);
    let mut sums: Vec<f32> = world.query::<&Pos>().map(|p| p.0).collect();
    sums.sort_by(f32::total_cmp);
    assert_eq!(sums, [2.0, 3.0, 3.0]);
}

#[test]
fn components_registered_later_are_noticed() {
    let mut world = World::new();
    let a = world.spawn((Pos(0.0),));
    // `Late` is unknown: nothing matches `&Late`, everything matches `Without<Late>`.
    let mut with_late = world.query_state::<&Pos, With<Late>>();
    let mut without_late = world.query_state::<Entity, Without<Late>>();
    let mut late = world.query_state::<&Late, ()>();
    assert_eq!(with_late.iter(&world).count(), 0);
    assert_eq!(without_late.iter(&world).count(), 1);
    assert_eq!(late.iter(&world).count(), 0);

    world.insert(a, Late);
    let b = world.spawn((Pos(1.0),));
    assert_eq!(with_late.iter(&world).count(), 1);
    assert_eq!(without_late.iter(&world).collect::<Vec<_>>(), [b]);
    assert_eq!(late.iter(&world).count(), 1);
}

#[test]
fn get_checks_the_match_and_the_filter() {
    let mut world = World::new();
    let moving = world.spawn((Pos(0.0), Vel(2.0)));
    let still = world.spawn((Pos(0.0),));
    let mut state = world.query_state::<(&mut Pos, &Vel), ()>();
    assert!(state.get_mut(&mut world, still).is_none());
    {
        let (mut p, v) = state.get_mut(&mut world, moving).unwrap();
        p.0 = v.0;
    }
    assert_eq!(world.get::<Pos>(moving), Some(&Pos(2.0)));

    world.clear_trackers();
    let mut changed = world.query_state::<Entity, Changed<Pos>>();
    assert!(changed.get(&world, moving).is_none());
    world.get_mut::<Pos>(moving).unwrap().0 = 5.0;
    assert_eq!(changed.get(&world, moving), Some(moving));
    world.despawn(moving);
    assert!(changed.get(&world, moving).is_none());
}

#[test]
fn cached_and_uncached_agree_across_many_archetypes() {
    #[derive(Clone, Copy)]
    struct Tag<const N: usize>;
    let mut world = World::new();
    let mut state: QueryState<(Entity, &Pos), Without<Frozen>> = world.query_state();
    for i in 0..200 {
        let e = world.spawn((Pos(i as f32),));
        match i % 5 {
            0 => world.insert(e, Tag::<0>),
            1 => world.insert(e, Tag::<1>),
            2 => world.insert(e, Frozen),
            3 => world.insert_bundle(e, (Tag::<2>, Vel(0.0))),
            _ => true,
        };
        if i % 7 == 0 {
            let mut cached: Vec<Entity> = state.iter(&world).map(|(e, _)| e).collect();
            let mut uncached: Vec<Entity> =
                world.query_filtered::<Entity, Without<Frozen>>().collect();
            cached.sort();
            uncached.sort();
            assert_eq!(cached, uncached);
        }
    }
    assert_eq!(state.iter(&world).count(), 160);
}

#[test]
#[should_panic(expected = "world other than its own")]
fn states_belong_to_their_world() {
    let world = World::new();
    let other = World::new();
    let mut state = world.query_state::<&Pos, ()>();
    state.iter(&other).count();
}
