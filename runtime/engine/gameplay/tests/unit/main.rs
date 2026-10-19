//! marlin-gameplay's tests (quick tier, docs/design/0014-testing.md §5): rotating movement and
//! spectators in a world made from the gameplay module. Run alone with
//! `cargo test -p marlin-gameplay --test unit`.

use marlin_asset::{AssetServer, Vfs};
use marlin_gameplay::{GameplayModule, RotatingMovement, SpectatorMovement};
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
        let mut modules =
            Modules::new(vec![Box::new(WorldModule), Box::new(GameplayModule)]).unwrap();
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
