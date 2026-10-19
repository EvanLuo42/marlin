//! marlin-gameplay's tests (quick tier, docs/design/0014-testing.md §5): rotating movement and
//! spectators in a world made from the gameplay module. Run alone with
//! `cargo test -p marlin-gameplay --test unit`.

use marlin_asset::{AssetServer, Vfs};
use std::sync::Arc;

use marlin_asset::{Link, Uuid};
use marlin_gameplay::{GameplayModule, RotatingMovement, SpectatorActions, SpectatorMovement};
use marlin_input::Key;
use marlin_input_action::{
    CompiledContext, InputFrame, InputModule, InputSource, KeyCode, Mapping, MappingContext,
    Modifier, Players, SwizzleOrder,
};
use marlin_jobs::{Config, JobSystem};
use marlin_math::{DVec3, Quat, Vec3};
use marlin_world::{
    EngineContext, Modules, Resources, TimeSettings, Transform, World, WorldKind, WorldModule,
};

struct Harness {
    modules: Modules,
    assets: AssetServer,
    vfs: Vfs,
    jobs: JobSystem,
    resources: Resources,
    world: World,
}

impl Harness {
    fn new() -> Self {
        let mut modules = Modules::new(vec![
            Box::new(WorldModule),
            Box::new(InputModule::new()),
            Box::new(GameplayModule::new()),
        ])
        .unwrap();
        let mut resources = Resources::new();
        let mut assets = AssetServer::new(modules.table().loaders(&resources));
        let vfs = Vfs::new();
        let jobs = JobSystem::new(Config {
            workers: 1,
            ..Config::default()
        })
        .unwrap();
        let mut world = modules.create_world(
            WorldKind::Game,
            &mut EngineContext {
                assets: &mut assets,
                vfs: &vfs,
                jobs: &jobs,
                resources: &mut resources,
            },
        );
        world.ecs_mut().insert_resource(TimeSettings::default());
        Self {
            modules,
            assets,
            vfs,
            jobs,
            resources,
            world,
        }
    }

    fn frame(&mut self, dt: f32) {
        self.modules.frame(
            &mut self.world,
            dt,
            &mut EngineContext {
                assets: &mut self.assets,
                vfs: &self.vfs,
                jobs: &self.jobs,
                resources: &mut self.resources,
            },
        );
    }
}

#[test]
fn rotating_movement_turns_from_where_it_started() {
    let mut h = Harness::new();
    let start = Quat::from_axis_angle(Vec3::X, 0.5);
    let e = h.world.ecs_mut().spawn((
        Transform::IDENTITY.with_rotation(start),
        RotatingMovement {
            axis: Vec3::Y,
            speed: 90.0,
        },
    ));
    // One second in steps: 90° about Y after the starting rotation.
    for _ in 0..10 {
        h.frame(0.1);
    }
    let want = start * Quat::from_axis_angle(Vec3::Y, core::f32::consts::FRAC_PI_2);
    let got = h.world.ecs().get::<Transform>(e).unwrap().rotation;
    assert!(got.rotation_eq(want, 1e-4), "{got:?}");
}

#[test]
fn spectators_hold_still_without_input() {
    let mut h = Harness::new();
    let at = Transform::from_translation(DVec3::new(1.0, 2.0, 3.0))
        .with_rotation(Quat::from_euler_yxz(0.3, -0.2, 0.0));
    let e = h.world.ecs_mut().spawn((at, SpectatorMovement::default()));
    for _ in 0..5 {
        h.frame(0.1);
    }
    assert_eq!(h.world.ecs().get::<Transform>(e), Some(&at));
}

#[test]
fn spectators_fly_by_player_zero_s_actions() {
    let mut h = Harness::new();
    // The content would hold these; here they are made by hand.
    let actions = SpectatorActions {
        context: Uuid::from_u128(10),
        movement: Uuid::from_u128(11),
        look: Uuid::from_u128(12),
        rise: Uuid::from_u128(13),
        boost: Uuid::from_u128(14),
    };
    let forward = Mapping {
        action: Link::new(actions.movement),
        source: InputSource::Key(KeyCode(Key::W)),
        modifiers: vec![Modifier::Swizzle(SwizzleOrder::Yxz)],
        triggers: Vec::new(),
    };
    let context = CompiledContext::compile(
        actions.context,
        &MappingContext {
            mappings: vec![forward],
        },
        |_| Some(marlin_input_action::ActionValueType::Axis2D),
    );
    h.world.ecs_mut().insert_resource(actions);
    let players = h.world.ecs_mut().resource_mut::<Players>().unwrap();
    players
        .player_mut(0)
        .unwrap()
        .user
        .add_context(Arc::new(context), 0);

    let e = h
        .world
        .ecs_mut()
        .spawn((Transform::IDENTITY, SpectatorMovement { speed: 10.0 }));
    for _ in 0..5 {
        let w = InputFrame::empty().with_key(Key::W);
        h.world
            .ecs_mut()
            .resource_mut::<Players>()
            .unwrap()
            .set_input(0, w);
        h.frame(0.1);
    }
    // Looking down -Z: forward is -Z, 10 m/s for half a second.
    let at = h.world.ecs().get::<Transform>(e).unwrap().translation;
    assert!(at.abs_diff_eq(DVec3::new(0.0, 0.0, -5.0), 1e-4), "{at:?}");
}
