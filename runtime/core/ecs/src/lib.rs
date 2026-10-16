//! Archetype entity component system.
//!
//! - **Storage**: entities with the same set of component types share an [`Archetype`], a table
//!   with one column per component (struct of arrays), so queries stream through contiguous
//!   memory. Adding or removing a component moves the entity to the matching archetype; the
//!   transitions are cached.
//! - **Queries**: `world.query::<(&mut Position, &Velocity, Option<&Tag>, Entity)>()`, filtered with
//!   [`With`] / [`Without`], or in parallel with [`World::par_for_each`] on the job system.
//! - **Systems**: [`System`]s declare what they read and write; a [`Schedule`] runs
//!   non-conflicting systems in parallel, keeps conflicting ones in order, and checks every access
//!   against the declaration. Structural changes go through [`Commands`].
//! - **Resources**: singletons stored by type ([`World::insert_resource`]).
//!
//! The VM and the editor see components through their reflected descriptions
//! ([`World::register_reflected`]). Change detection and query caching are not implemented yet.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod archetype;
mod bundle;
mod command;
mod component;
mod entity;
mod query;
mod schedule;
mod storage;
mod world;

pub use archetype::{Archetype, ArchetypeId};
pub use bundle::{Bundle, MAX_BUNDLE};
pub use command::Commands;
pub use component::{Component, ComponentId, ComponentInfo, Components};
pub use entity::Entity;
pub use query::{Access, QueryData, QueryFilter, QueryIter, ReadOnlyQueryData, With, Without};
pub use schedule::{Schedule, System, SystemContext, SystemWorld};
pub use world::World;
