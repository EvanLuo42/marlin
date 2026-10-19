//! Engine modules: how subsystems join a build (0019 §3).
//!
//! A [`Module`] registers what it brings ([`Registry`]: component types, systems in tick groups,
//! asset loaders, cvars) before any world exists; [`Modules::new`] orders the modules by their
//! dependencies, runs every registration and freezes the result into a [`ModuleTable`], from
//! which any number of worlds are made alike. Modules then start (engine-level state, in
//! [`Resources`]), set up each world (its resources), and see each world's frame from outside
//! (`pre_world_frame`, `post_world_frame`) for the work that needs engine-level state, such as
//! loading assets.
//!
//! Modules do not call each other: they share data through resources (engine-level ones in
//! [`Resources`], a world's in its ECS world) and components.
//!
//! The set of modules is fixed per build target (game, editor, server) and listed in the
//! umbrella crate; projects do not switch modules on or off (0019 §3.2).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::{Any, TypeId};

use marlin_asset::{AssetServer, Loaders, Vfs};
use marlin_config::Cvar;
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::System;
use marlin_jobs::JobSystem;

use crate::registry::ComponentRegistry;
use crate::schedule::TickGroup;
use crate::world::{World, WorldKind};

/// Engine-level state shared by modules and the host, by type: the asset streamer, a module's
/// loaded engine assets, the GPU device.
#[derive(Default)]
pub struct Resources {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Resources {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Adds or replaces the resource of type `R`.
    pub fn insert<R: Any + Send + Sync>(&mut self, resource: R) {
        self.map.insert(TypeId::of::<R>(), Box::new(resource));
    }

    pub fn get<R: Any + Send + Sync>(&self) -> Option<&R> {
        self.map.get(&TypeId::of::<R>())?.downcast_ref()
    }

    pub fn get_mut<R: Any + Send + Sync>(&mut self) -> Option<&mut R> {
        self.map.get_mut(&TypeId::of::<R>())?.downcast_mut()
    }

    pub fn remove<R: Any + Send + Sync>(&mut self) -> Option<R> {
        let boxed = self.map.remove(&TypeId::of::<R>())?;
        boxed.downcast().ok().map(|b| *b)
    }

    pub fn contains<R: Any + Send + Sync>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<R>())
    }
}

/// What module hooks get: the engine's assets, its content, its job system and its resources.
pub struct EngineContext<'a, 'v> {
    pub assets: &'a mut AssetServer,
    pub vfs: &'v Vfs,
    pub jobs: &'a JobSystem,
    pub resources: &'a mut Resources,
}

/// A module that failed to start.
#[derive(Debug, marlin_error::Error)]
#[error("module `{module}`: {message}")]
pub struct ModuleError {
    pub module: &'static str,
    pub message: String,
}

impl ModuleError {
    pub fn new(module: &'static str, message: impl Into<String>) -> Self {
        Self {
            module,
            message: message.into(),
        }
    }
}

/// A subsystem's part in the engine (0019 §3.1). Only [`name`](Self::name) and
/// [`register`](Self::register) are required.
pub trait Module: Any + Send {
    /// Unique: dependencies name it, logs and errors say it.
    fn name(&self) -> &'static str;

    /// Modules that must register and start first.
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }

    /// Registers what the module brings. Pure data: no GPU, no IO.
    fn register(&self, registry: &mut Registry);

    /// Engine-level start: content is mounted and the assets can be loaded. Puts the module's
    /// engine-level state in `cx.resources`.
    fn startup(&mut self, _cx: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        Ok(())
    }

    /// A new world: inserts the module's world resources.
    fn init_world(&mut self, _world: &mut World, _cx: &mut EngineContext<'_, '_>) {}

    /// Before a world's frame, with engine-level state at hand (loading what the world asked
    /// for).
    fn pre_world_frame(&mut self, _world: &mut World, _cx: &mut EngineContext<'_, '_>) {}

    /// After a world's frame (its extract has run): finishing what needs engine-level state.
    fn post_world_frame(&mut self, _world: &mut World, _cx: &mut EngineContext<'_, '_>) {}

    /// Whether the module has caught up with the world (every asset it asked for is loaded and
    /// handed on). Hosts wait for every module before reporting a level loaded.
    fn world_ready(&self, _world: &World) -> bool {
        true
    }

    fn shutdown(&mut self, _cx: &mut EngineContext<'_, '_>) {}
}

/// Makes a system for each world (systems hold per-world state, so each world gets its own).
pub type SystemFactory = Arc<dyn Fn() -> System + Send + Sync>;

/// Adds a module's loaders, with the engine's resources at hand (the blob streamer).
pub type LoaderFactory = Arc<dyn Fn(&mut Loaders, &Resources) + Send + Sync>;

/// What modules register (0019 §3.1).
#[derive(Default)]
pub struct Registry {
    components: ComponentRegistry,
    systems: Vec<(TickGroup, &'static str, SystemFactory)>,
    loaders: Vec<LoaderFactory>,
    cvars: Vec<&'static Cvar>,
    /// The module registering.
    module: &'static str,
}

impl Registry {
    pub fn components(&mut self) -> &mut ComponentRegistry {
        &mut self.components
    }

    /// A system in `group` for every world, made by `make`.
    pub fn add_system(
        &mut self,
        group: TickGroup,
        make: impl Fn() -> System + Send + Sync + 'static,
    ) -> &mut Self {
        self.systems.push((group, self.module, Arc::new(make)));
        self
    }

    /// Asset loaders, added to every asset server the engine makes.
    pub fn add_loaders(
        &mut self,
        add: impl Fn(&mut Loaders, &Resources) + Send + Sync + 'static,
    ) -> &mut Self {
        self.loaders.push(Arc::new(add));
        self
    }

    /// Cvars, registered with `marlin-config` when the modules are.
    pub fn add_cvars(&mut self, cvars: &[&'static Cvar]) -> &mut Self {
        self.cvars.extend_from_slice(cvars);
        self
    }
}

/// Everything the modules registered, frozen: worlds and asset servers are made from it.
pub struct ModuleTable {
    components: Arc<ComponentRegistry>,
    systems: Vec<(TickGroup, &'static str, SystemFactory)>,
    loaders: Vec<LoaderFactory>,
}

impl ModuleTable {
    pub fn components(&self) -> &Arc<ComponentRegistry> {
        &self.components
    }

    /// Every module's loaders.
    pub fn loaders(&self, resources: &Resources) -> Loaders {
        let mut loaders = Loaders::new();
        for add in &self.loaders {
            add(&mut loaders, resources);
        }
        loaders
    }

    /// A world of `kind` with every module's components and systems (and none of their world
    /// resources: [`Modules::create_world`] adds those).
    pub fn new_world(&self, kind: WorldKind) -> World {
        let mut world = World::new(kind, self.components.clone());
        for (group, _, make) in &self.systems {
            world.add_system(*group, make());
        }
        world
    }

    /// The systems in each group, by module, in registration order: for logs and tests.
    pub fn systems(&self) -> impl Iterator<Item = (TickGroup, &'static str)> + '_ {
        self.systems.iter().map(|(g, m, _)| (*g, *m))
    }
}

/// The modules of a build, in dependency order, and their table.
pub struct Modules {
    list: Vec<Box<dyn Module>>,
    table: ModuleTable,
}

impl Modules {
    /// Orders `modules` so each comes after its dependencies (keeping the given order where
    /// it is free) and runs their registration.
    ///
    /// # Errors
    /// Two modules with one name, a dependency on a module not in the list, or a cycle.
    pub fn new(modules: Vec<Box<dyn Module>>) -> Result<Self, ModuleError> {
        let names: Vec<&'static str> = modules.iter().map(|m| m.name()).collect();
        for (i, name) in names.iter().enumerate() {
            if names[..i].contains(name) {
                return Err(ModuleError::new(name, "listed twice"));
            }
        }
        for m in &modules {
            for dep in m.dependencies() {
                if !names.contains(dep) {
                    return Err(ModuleError::new(
                        m.name(),
                        format!("depends on `{dep}`, which this build does not have"),
                    ));
                }
            }
        }
        let mut pending: Vec<Option<Box<dyn Module>>> = modules.into_iter().map(Some).collect();
        let mut list: Vec<Box<dyn Module>> = Vec::with_capacity(pending.len());
        while list.len() < pending.len() {
            let next = pending.iter().position(|m| {
                m.as_ref().is_some_and(|m| {
                    m.dependencies()
                        .iter()
                        .all(|d| list.iter().any(|done| done.name() == *d))
                })
            });
            match next {
                Some(i) => list.push(pending[i].take().expect("pending")),
                None => {
                    let stuck = pending.iter().flatten().next().expect("one left");
                    return Err(ModuleError::new(
                        stuck.name(),
                        "its dependencies form a cycle",
                    ));
                }
            }
        }

        let mut registry = Registry::default();
        for m in &list {
            registry.module = m.name();
            m.register(&mut registry);
        }
        let _ = marlin_config::register_all(&registry.cvars);
        Ok(Self {
            list,
            table: ModuleTable {
                components: Arc::new(registry.components),
                systems: registry.systems,
                loaders: registry.loaders,
            },
        })
    }

    pub fn table(&self) -> &ModuleTable {
        &self.table
    }

    /// The module names, in dependency order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.list.iter().map(|m| m.name())
    }

    pub fn has(&self, name: &str) -> bool {
        self.list.iter().any(|m| m.name() == name)
    }

    /// The module of type `M`.
    pub fn get<M: Module>(&self) -> Option<&M> {
        self.list
            .iter()
            .find_map(|m| (m.as_ref() as &dyn Any).downcast_ref())
    }

    pub fn get_mut<M: Module>(&mut self) -> Option<&mut M> {
        self.list
            .iter_mut()
            .find_map(|m| (m.as_mut() as &mut dyn Any).downcast_mut())
    }

    /// Starts every module, in order.
    ///
    /// # Errors
    /// The first module that fails; those after it are not started.
    pub fn startup(&mut self, cx: &mut EngineContext<'_, '_>) -> Result<(), ModuleError> {
        for m in &mut self.list {
            m.startup(cx)?;
        }
        Ok(())
    }

    /// A world of `kind`, set up by every module.
    pub fn create_world(&mut self, kind: WorldKind, cx: &mut EngineContext<'_, '_>) -> World {
        let mut world = self.table.new_world(kind);
        for m in &mut self.list {
            m.init_world(&mut world, cx);
        }
        world
    }

    /// One frame of `world`, `interval` seconds after its last, between the modules' hooks.
    pub fn frame(&mut self, world: &mut World, interval: f32, cx: &mut EngineContext<'_, '_>) {
        for m in &mut self.list {
            m.pre_world_frame(world, cx);
        }
        world.frame(interval, cx.jobs);
        for m in &mut self.list {
            m.post_world_frame(world, cx);
        }
    }

    /// Whether every module has caught up with `world`.
    pub fn world_ready(&self, world: &World) -> bool {
        self.list.iter().all(|m| m.world_ready(world))
    }

    /// Shuts the modules down, in reverse order.
    pub fn shutdown(&mut self, cx: &mut EngineContext<'_, '_>) {
        for m in self.list.iter_mut().rev() {
            m.shutdown(cx);
        }
    }
}
