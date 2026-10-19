//! [`Engine`]: the simulation side of the host (0019 §12): the modules, the asset server and
//! the engine-level resources. Worlds are the caller's: [`Engine::create_world`] makes one with
//! every module's components, systems and resources, and [`Engine::frame`] runs a frame of it
//! between the modules' hooks.

use alloc::sync::Arc;

use marlin_asset::{AssetServer, BlobStreamer, Loaders, Vfs};
use marlin_io::IoQueue;
use marlin_jobs::{Config, JobSystem};
use marlin_world::{EngineContext, ModuleError, Modules, Resources, World, WorldKind};

#[derive(Debug, marlin_error::Error)]
pub enum EngineError {
    #[error("asset IO")]
    Io(#[from] marlin_io::Error),
    #[error("{0}")]
    Module(#[from] ModuleError),
}

pub struct Engine<'v> {
    vfs: &'v Vfs,
    io: IoQueue<'v>,
    pub assets: AssetServer,
    /// Reads the streamed blobs (mesh pages) renderers ask for, on its own queue. Also an engine
    /// resource, for the loaders.
    pub streamer: Arc<BlobStreamer>,
    stream_io: IoQueue<'v>,
    resources: Resources,
    modules: Modules,
    /// For callers that pass no job system.
    own_jobs: Option<JobSystem>,
}

fn fallback_jobs() -> JobSystem {
    JobSystem::new(Config {
        workers: 1,
        ..Config::default()
    })
    .expect("a job system")
}

impl<'v> Engine<'v> {
    /// An engine of `modules` (registered: their cvars exist before the boot configuration is
    /// read) loading from `vfs`: their loaders in the asset server, and each started in dependency
    /// order. `jobs` runs the startup loads (a one-worker job system of the engine's own without
    /// it).
    pub fn new(
        vfs: &'v Vfs,
        mut modules: Modules,
        jobs: Option<&JobSystem>,
    ) -> Result<Self, EngineError> {
        let streamer = Arc::new(BlobStreamer::new());
        let mut resources = Resources::new();
        resources.insert(streamer.clone());
        let mut assets = AssetServer::new(modules.table().loaders(&resources));
        let own_jobs = jobs.is_none().then(fallback_jobs);
        modules.startup(&mut EngineContext {
            assets: &mut assets,
            vfs,
            jobs: jobs.or(own_jobs.as_ref()).expect("one or the other"),
            resources: &mut resources,
        })?;
        Ok(Self {
            vfs,
            io: IoQueue::new(64)?,
            assets,
            streamer,
            stream_io: IoQueue::new(64)?,
            resources,
            modules,
            own_jobs,
        })
    }

    pub fn vfs(&self) -> &'v Vfs {
        self.vfs
    }

    pub fn modules(&self) -> &Modules {
        &self.modules
    }

    /// The engine-level resources: what the modules started with (`RenderAssets`, ...).
    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    /// A fresh set of every module's loaders, for another asset server (tools that load on
    /// their own while the engine's is busy).
    pub fn loaders(&self) -> Loaders {
        self.modules.table().loaders(&self.resources)
    }

    /// A world of `kind`, set up by every module.
    pub fn create_world(&mut self, kind: WorldKind, jobs: Option<&JobSystem>) -> World {
        if jobs.is_none() && self.own_jobs.is_none() {
            self.own_jobs = Some(fallback_jobs());
        }
        let mut cx = EngineContext {
            assets: &mut self.assets,
            vfs: self.vfs,
            jobs: jobs.or(self.own_jobs.as_ref()).expect("one or the other"),
            resources: &mut self.resources,
        };
        self.modules.create_world(kind, &mut cx)
    }

    /// One frame of `world`, `dt` seconds after its last: the engine's loads and streaming,
    /// then the world's frame between the modules' hooks. Input reaches the world's players
    /// through routing first ([`crate::input`]). `jobs` runs the world's systems and
    /// decompresses loads.
    pub fn frame(&mut self, world: &mut World, dt: f32, jobs: Option<&JobSystem>) {
        if jobs.is_none() && self.own_jobs.is_none() {
            self.own_jobs = Some(fallback_jobs());
        }
        let jobs = jobs.or(self.own_jobs.as_ref()).expect("one or the other");
        self.assets.update(self.vfs, &mut self.io, Some(jobs));
        self.streamer.update(self.vfs, &mut self.stream_io);
        let mut cx = EngineContext {
            assets: &mut self.assets,
            vfs: self.vfs,
            jobs,
            resources: &mut self.resources,
        };
        self.modules.frame(world, dt, &mut cx);
    }

    /// Whether `world`'s levels have spawned and every module has caught up with it (its
    /// meshes loaded and sent): what hosts wait for before reporting a level loaded.
    pub fn is_ready(&self, world: &World) -> bool {
        self.modules.world_ready(world)
    }

    /// Shuts the modules down (in reverse order).
    pub fn shutdown(mut self, jobs: &JobSystem) {
        let mut cx = EngineContext {
            assets: &mut self.assets,
            vfs: self.vfs,
            jobs,
            resources: &mut self.resources,
        };
        self.modules.shutdown(&mut cx);
    }
}
