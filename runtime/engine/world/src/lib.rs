//! The world: entities and components on `marlin-ecs`, the frame's tick groups, simulation time,
//! the transform hierarchy and stable entity identity (docs/design/0019-engine-framework-and-world.md).
//!
//! - [`World`]: an ECS world of a [`WorldKind`] (game, editor, PIE, preview) with its
//!   [`WorldSchedule`]. [`World::frame`] runs the [`TickGroup`]s in order; the physics groups run
//!   once per fixed step ([`FixedTime`]), and [`Interpolated`] entities are drawn between steps.
//! - [`Transform`] (relative to the parent, `f64` translation) and [`GlobalTransform`] (in the
//!   world), kept in step by [`propagate_transforms`] in `TransformPropagate`. Code working in
//!   `f32` takes positions relative to an origin near it ([`GlobalTransform::relative_to`]).
//! - [`ComponentRegistry`]: the component types worlds know, with what each may be used for
//!   ([`ComponentFlags`]: saved in levels, replicated). Shared by every world of a process.
//! - [`EntityGuid`]: an entity's identity in data; [`EntityRef`] refers to one, and the world's
//!   [`GuidIndex`] resolves it to the live [`Entity`](marlin_ecs::Entity).
//!
//! - Levels: [`Level`] and [`Prefab`] as authored ([`EntityDesc`]s of [`ComponentData`],
//!   [`PrefabInstance`]s with [`Override`]s), [`flatten`]ed and cooked to [`CookedLevel`]s, which
//!   the world spawns as sublevels ([`World::spawn_level`], [`World::load_sublevel`],
//!   [`World::unload_sublevel`]) or Prefab instances ([`World::spawn_prefab`]).
//!
//! - Modules ([`Module`], [`Modules`]): how subsystems join a build: they register components,
//!   systems, loaders and cvars, start with the engine and see each world's frame. [`WorldModule`]
//!   is this crate's: it loads the level assets sublevels ask for.
//!
//! Subsystems (render, physics, audio, scripts) depend on this crate, never the reverse: they
//! register their components and put their systems in tick groups.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod component_data;
mod guid;
mod level;
mod levels;
mod module;
mod registry;
mod schedule;
mod time;
mod transform;
mod world;
mod world_module;

pub use component_data::{ComponentData, IsComponent};
pub use guid::{EntityGuid, EntityRef, GuidIndex};
pub use level::{
    CookedArchetype, CookedEntity, CookedLevel, EntityDesc, FlatEntity, FlattenError, Level,
    NoPrefabs, Override, Prefab, PrefabInstance, PrefabSource, flatten, flatten_prefab,
};
pub use levels::{
    EntityName, InLevel, LevelRequest, Levels, SublevelId, SublevelState, W_SPAWN_BUDGET_MS,
};
pub use module::{
    EngineContext, LoaderFactory, Module, ModuleError, ModuleTable, Modules, Registry, Resources,
    SystemFactory,
};
pub use registry::{ComponentFlags, ComponentRegistration, ComponentRegistry};
pub use schedule::{TickGroup, WorldSchedule};
pub use time::{FixedTime, Interpolated, T_FIXED_HZ, T_MAX_DT, T_MAX_SUBSTEPS, Time, TimeSettings};
pub use transform::{GlobalTransform, Transform, propagate_transforms};
pub use world::{NetRole, World, WorldKind};
pub use world_module::WorldModule;
