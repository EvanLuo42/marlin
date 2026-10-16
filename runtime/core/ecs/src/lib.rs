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
//! - **Change detection** ([`change`]): writes through [`Mut`] stamp the row; [`Changed`] and
//!   [`Added`] filter by it, [`Ref`] reads it, and [`World::removed`] lists removals. Systems see
//!   what changed since their previous run; queries through the world, since the last
//!   [`World::clear_trackers`].
//!
//! The VM and the editor see components through their reflected descriptions
//! ([`World::register_reflected`]). Query caching is not implemented yet.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod archetype;
mod bundle;
pub mod change;
mod command;
mod component;
mod entity;
mod query;
mod schedule;
mod storage;
mod world;

pub use archetype::{Archetype, ArchetypeId};
pub use bundle::{Bundle, MAX_BUNDLE};
pub use change::{Added, ChangeTicks, Changed, Mut, Ref, Tick};
pub use command::Commands;
pub use component::{Component, ComponentId, ComponentInfo, Components};
pub use entity::Entity;
pub use query::{Access, QueryData, QueryFilter, QueryIter, ReadOnlyQueryData, With, Without};
pub use schedule::{Schedule, System, SystemContext, SystemWorld};
pub use storage::ComponentTicks;
pub use world::World;
