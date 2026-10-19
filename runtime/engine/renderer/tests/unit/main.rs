//! marlin-renderer's platform-independent tests (quick tier, docs/design/0014-testing.md §5):
//! the extract (scene updates and views) and the render module's engine assets from mounted
//! content. Run alone with `cargo test -p marlin-renderer --test unit`.

mod assets;
mod extract;

use marlin_asset::{AssetServer, Vfs};
use marlin_jobs::{Config, JobSystem};
use marlin_renderer::{RenderDelta, RenderModule};
use marlin_world::{EngineContext, Modules, Resources, World, WorldKind, WorldModule};

/// The world and render modules, registered (not started: no shaders here).
pub fn modules() -> Modules {
    Modules::new(vec![Box::new(WorldModule), Box::new(RenderModule)]).unwrap()
}

/// A game world of the world and render modules, run a frame at a time as an engine would.
pub struct Harness {
    pub modules: Modules,
    pub assets: AssetServer,
    pub vfs: Vfs,
    pub jobs: JobSystem,
    pub resources: Resources,
    pub world: World,
}

impl Harness {
    #[allow(clippy::new_without_default, reason = "a test harness")]
    pub fn new() -> Self {
        let mut modules = modules();
        let resources = Resources::new();
        let mut assets = AssetServer::new(modules.table().loaders(&resources));
        let vfs = Vfs::new();
        let jobs = JobSystem::new(Config {
            workers: 1,
            ..Config::default()
        })
        .unwrap();
        let mut resources = resources;
        let world = modules.create_world(
            WorldKind::Game,
            &mut EngineContext {
                assets: &mut assets,
                vfs: &vfs,
                jobs: &jobs,
                resources: &mut resources,
            },
        );
        Self {
            modules,
            assets,
            vfs,
            jobs,
            resources,
            world,
        }
    }

    /// One frame (time standing still); its delta.
    pub fn frame(&mut self) -> RenderDelta {
        self.modules.frame(
            &mut self.world,
            0.0,
            &mut EngineContext {
                assets: &mut self.assets,
                vfs: &self.vfs,
                jobs: &self.jobs,
                resources: &mut self.resources,
            },
        );
        marlin_renderer::take_delta(&mut self.world).expect("a frame leaves a delta")
    }
}
