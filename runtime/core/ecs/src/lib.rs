//! Archetype entity component system.
//!
//! - **Storage**: entities with the same set of component types share an [`Archetype`]. Their
//!   components live in a [`Table`], one column per component (struct of arrays), so queries
//!   stream through contiguous memory; adding or removing a component moves the entity to the
//!   matching archetype, and the transitions (per component and per bundle type) are cached.
//!   Components registered with [`StorageType::SparseSet`] live in one map per type instead:
//!   adding and removing them only changes the entity's archetype, not its table row.
//! - **Queries**: `world.query::<(&mut Position, &Velocity, Option<&Tag>, Entity)>()`, filtered with
//!   [`With`] / [`Without`], or in parallel with [`World::par_for_each`] on the job system. A
//!   [`QueryState`] caches the matching archetypes and only looks at new ones.
//! - **Systems**: functions whose parameters ([`Query`], [`Res`], [`ResMut`], [`Commands`],
//!   [`Local`], [`EventReader`], ...) say what they access, or closures that declare it
//!   ([`System`]). A [`Schedule`] runs non-conflicting systems in parallel, keeps conflicting ones
//!   in order, honours explicit ordering, sets and run conditions, and checks every access
//!   against the declaration. Structural changes go through [`Commands`].
//! - **Resources**: singletons stored by type ([`World::insert_resource`]); non-`Send` ones stay
//!   on their thread ([`World::insert_non_send`]).
//! - **Change detection** ([`change`]): writes through [`Mut`] stamp the row; [`Changed`] and
//!   [`Added`] filter by it, [`Ref`] reads it, and [`World::removed`] lists removals. Systems see
//!   what changed since their previous run; queries through the world, since the last
//!   [`World::clear_trackers`].
//! - **Events** ([`Events`]), **component hooks** ([`ComponentHooks`]) and a parent/child
//!   **hierarchy** ([`hierarchy`]).
//!
//! The VM and the editor see components through their ids, raw bytes and reflected descriptions
//! ([`World::register_reflected`], [`World::get_by_id`], [`DynamicQuery`]).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod archetype;
mod bundle;
pub mod change;
mod command;
mod component;
mod dynamic;
mod entity;
mod event;
pub mod hierarchy;
mod query;
mod schedule;
mod storage;
pub mod system;
mod world;

pub use archetype::{Archetype, ArchetypeId};
pub use bundle::{Bundle, MAX_BUNDLE};
pub use change::{Added, ChangeTicks, Changed, Mut, Ref, Tick};
pub use command::{CommandQueue, Commands};
pub use component::{
    CloneFn, Component, ComponentDescriptor, ComponentHook, ComponentHooks, ComponentId,
    ComponentInfo, Components, HookContext, StorageType,
};
pub use dynamic::{ComponentMut, ComponentPtr, DynamicQuery, DynamicQueryBuilder, DynamicRow};
pub use entity::Entity;
pub use event::{Event, EventReader, EventWriter, Events, update_events};
pub use hierarchy::{Children, Parent};
pub use query::{
    Access, AccessKey, QueryData, QueryFilter, QueryIter, QueryState, ReadOnlyQueryData, With,
    Without,
};
pub use schedule::{IntoSystem, Schedule, SetConfig, System, SystemContext, SystemWorld};
pub use storage::{ComponentTicks, Table, TableId};
pub use system::{
    Condition, IntoCondition, Local, Query, ReadOnlySystemParam, RemovedComponents, Res, ResMut,
    SystemParam, SystemParamFunction, not, resource_exists,
};
pub use world::{World, WorldId};
