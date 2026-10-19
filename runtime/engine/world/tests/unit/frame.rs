//! The frame: tick group order, which groups each kind of world runs, time, fixed steps and
//! interpolation.

use std::sync::{Arc, Mutex};

use marlin_ecs::{Query, System};
use marlin_math::{DVec3, Quat};
use marlin_world::{
    FixedTime, Interpolated, TickGroup, Time, TimeSettings, Transform, World, WorldKind,
};

use crate::{jobs, world};

/// Adds a system to every group that logs the group's name.
fn log_groups(w: &mut World) -> Arc<Mutex<Vec<&'static str>>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    for group in TickGroup::ALL {
        let log = log.clone();
        w.add_system(
            group,
            System::new(group.name(), move |_| {
                log.lock().unwrap().push(group.name())
            }),
        );
    }
    log
}

#[test]
fn groups_run_in_order_with_the_fixed_groups_once_per_step() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    let log = log_groups(&mut w);
    // Two and a bit steps at 60 Hz.
    w.frame(2.2 / 60.0, &jobs);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "FrameStart",
            "PrePhysics",
            "FixedPrePhysics",
            "Physics",
            "FixedPostPhysics",
            "FixedPrePhysics",
            "Physics",
            "FixedPostPhysics",
            "PostPhysics",
            "LateUpdate",
            "TransformPropagate",
            "Extract",
            "FrameEnd",
        ]
    );
    assert_eq!(w.fixed_time().tick(), 2);
}

#[test]
fn editor_and_paused_worlds_skip_gameplay() {
    let jobs = jobs();
    let expected = ["FrameStart", "TransformPropagate", "Extract", "FrameEnd"];

    let mut editor = world(WorldKind::Editor);
    let log = log_groups(&mut editor);
    editor.frame(0.05, &jobs);
    assert_eq!(*log.lock().unwrap(), expected);

    let mut game = world(WorldKind::Game);
    let log = log_groups(&mut game);
    game.time_mut().paused = true;
    game.frame(0.05, &jobs);
    assert_eq!(*log.lock().unwrap(), expected);
    assert_eq!(game.time().delta(), 0.0);
    assert_eq!(game.time().unscaled_delta(), 0.05);
    assert_eq!(game.fixed_time().tick(), 0);
}

#[test]
fn time_clamps_scales_and_counts() {
    let mut t = Time::new();
    t.advance(0.5, 0.1);
    assert_eq!(t.delta(), 0.1);
    t.scale = 0.5;
    t.advance(0.02, 0.1);
    assert_eq!(t.delta(), 0.01);
    assert_eq!(t.unscaled_delta(), 0.02);
    assert_eq!(t.frame(), 2);
    assert!((t.elapsed() - 0.11).abs() < 1e-6);
}

#[test]
fn fixed_time_banks_remainders_and_drops_spirals() {
    let settings = TimeSettings {
        max_delta: 1.0,
        fixed_step: 0.01,
        max_substeps: 4,
    };
    let mut f = FixedTime::new(0.01);
    // 6 ms frames: no step, then one (12 ms banked), then none (8 ms banked).
    assert_eq!(f.accumulate(0.006, &settings), 0);
    assert_eq!(f.accumulate(0.006, &settings), 1);
    assert_eq!(f.accumulate(0.006, &settings), 0);
    f.finish_frame();
    assert!((f.alpha() - 0.8).abs() < 1e-4, "{}", f.alpha());
    // A 1 s hitch runs four steps and drops the rest instead of owing 96 more.
    assert_eq!(f.accumulate(1.0, &settings), 4);
    assert_eq!(f.accumulate(0.0, &settings), 0);
}

#[test]
fn interpolated_motion_is_drawn_between_steps() {
    let jobs = jobs();
    let mut w = world(WorldKind::Game);
    // Moves 1 m per fixed step.
    w.add_system(
        TickGroup::Physics,
        |mut q: Query<'_, '_, &mut Transform>| {
            for mut t in q.iter_mut() {
                t.translation.x += 1.0;
            }
        },
    );
    let e = w
        .ecs_mut()
        .spawn((Transform::default(), Interpolated::default()));
    // One and a half steps: one runs, half a step is banked.
    w.frame(1.5 / 60.0, &jobs);
    let alpha = w.fixed_time().alpha();
    assert!((alpha - 0.5).abs() < 1e-3, "{alpha}");
    let i = *w.ecs().get::<Interpolated>(e).unwrap();
    assert_eq!(
        i.previous.translation,
        DVec3::ZERO,
        "starts at rest where it spawned"
    );
    assert_eq!(i.current.translation, DVec3::new(1.0, 0.0, 0.0));
    assert!(
        i.at(alpha)
            .translation
            .abs_diff_eq(DVec3::new(0.5, 0.0, 0.0), 1e-3)
    );
    assert_eq!(i.at(alpha).rotation, Quat::IDENTITY);
}
