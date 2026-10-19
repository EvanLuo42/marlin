//! Transform propagation: hierarchy composition, precision far away, and writing only what moved.

use std::sync::{Arc, Mutex};

use marlin_ecs::{Changed, Entity};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_world::{GlobalTransform, TickGroup, Transform, World, WorldKind};

use crate::{jobs, world};

const EPS: f64 = 1e-5;

fn global(w: &World, e: Entity) -> GlobalTransform {
    *w.ecs()
        .get::<GlobalTransform>(e)
        .expect("has a global transform")
}

/// Records, each frame in `Extract`, the entities whose global transform was written.
fn moved_in_extract(w: &mut World) -> Arc<Mutex<Vec<Entity>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    w.add_system(TickGroup::Extract, move |world: &mut marlin_ecs::World| {
        let mut v: Vec<Entity> = world
            .query_ref_filtered::<Entity, Changed<GlobalTransform>>()
            .collect();
        v.sort();
        *out.lock().unwrap() = v;
    });
    seen
}

#[test]
fn children_compose_with_their_parents() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let turn = Quat::from_axis_angle(Vec3::Y, core::f32::consts::FRAC_PI_2);
    let root = w.ecs_mut().spawn((Transform {
        translation: DVec3::new(10.0, 0.0, 0.0),
        rotation: turn,
        scale: Vec3::splat(2.0),
    },));
    let child = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(1.0, 0.0, 0.0)),));
    let grandchild = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(0.0, 3.0, 0.0)),));
    w.ecs_mut().set_parent(child, root);
    w.ecs_mut().set_parent(grandchild, child);
    w.frame(0.016, &jobs);

    assert!(
        global(&w, root)
            .translation
            .abs_diff_eq(DVec3::new(10.0, 0.0, 0.0), EPS)
    );
    // (1, 0, 0) scaled by 2, turned 90° about Y: (0, 0, -2).
    let c = global(&w, child);
    assert!(
        c.translation.abs_diff_eq(DVec3::new(10.0, 0.0, -2.0), EPS),
        "{c:?}"
    );
    assert_eq!(c.scale, Vec3::splat(2.0));
    let g = global(&w, grandchild);
    assert!(
        g.translation.abs_diff_eq(DVec3::new(10.0, 6.0, -2.0), EPS),
        "{g:?}"
    );
    assert!(g.rotation.rotation_eq(turn, 1e-6));
}

#[test]
fn far_from_the_origin_children_keep_millimeters() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let far = DVec3::new(8.0e6, 1.0e3, -6.0e6);
    let root = w.ecs_mut().spawn((Transform::from_translation(far),));
    let child = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(0.001, 0.002, 0.003)),));
    w.ecs_mut().set_parent(child, root);
    w.frame(0.016, &jobs);
    let rel = global(&w, child).relative_to(far).translation;
    assert!(
        (rel - Vec3::new(0.001, 0.002, 0.003)).abs().max_element() < 1e-7,
        "{rel:?}"
    );
}

#[test]
fn only_what_moved_is_written() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let seen = moved_in_extract(&mut w);
    let a = w.ecs_mut().spawn((Transform::default(),));
    let a_child = w.ecs_mut().spawn((Transform::default(),));
    let b = w.ecs_mut().spawn((Transform::default(),));
    w.ecs_mut().set_parent(a_child, a);
    w.frame(0.016, &jobs);
    let mut all = vec![a, a_child, b];
    all.sort();
    assert_eq!(
        *seen.lock().unwrap(),
        all,
        "everything is new on the first frame"
    );

    w.frame(0.016, &jobs);
    assert!(seen.lock().unwrap().is_empty(), "nothing moved");

    w.ecs_mut().get_mut::<Transform>(a).unwrap().translation.x = 5.0;
    w.frame(0.016, &jobs);
    let mut moved = vec![a, a_child];
    moved.sort();
    assert_eq!(*seen.lock().unwrap(), moved, "a moves its child, not b");
    assert_eq!(global(&w, a_child).translation.x, 5.0);
}

#[test]
fn reparenting_and_unparenting_move_the_entity() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let p1 = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(100.0, 0.0, 0.0)),));
    let p2 = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(0.0, 100.0, 0.0)),));
    let c = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(1.0, 0.0, 0.0)),));
    w.ecs_mut().set_parent(c, p1);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(101.0, 0.0, 0.0));

    w.ecs_mut().set_parent(c, p2);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(1.0, 100.0, 0.0));

    w.ecs_mut().remove_parent(c);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(1.0, 0.0, 0.0));

    // The parent despawning also leaves it in world space.
    w.ecs_mut().set_parent(c, p1);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(101.0, 0.0, 0.0));
    w.ecs_mut().despawn(p1);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(1.0, 0.0, 0.0));
}

#[test]
fn an_ancestor_without_a_transform_breaks_the_chain() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let group = w.ecs_mut().spawn_empty();
    let c = w
        .ecs_mut()
        .spawn((Transform::from_translation(DVec3::new(2.0, 0.0, 0.0)),));
    w.ecs_mut().set_parent(c, group);
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, c).translation, DVec3::new(2.0, 0.0, 0.0));
}

#[test]
fn editor_worlds_place_without_simulating() {
    let jobs = jobs();
    let mut w = world(WorldKind::Editor);
    let e = w.ecs_mut().spawn((Transform::default(),));
    w.frame(0.016, &jobs);
    w.ecs_mut().get_mut::<Transform>(e).unwrap().translation.y = 3.0;
    w.frame(0.016, &jobs);
    assert_eq!(global(&w, e).translation.y, 3.0);
}
