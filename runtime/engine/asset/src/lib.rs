//! Assets at runtime: identity, cooked units, packages, mounting and loading
//! (`docs/design/0003-asset-pipeline.md` §7–§8).
//!
//! - Identity: [`Uuid`], [`Link`], [`SourceFile`].
//! - [`cooked`]: the cooked unit format the cooker writes and the runtime reads in place.
//! - [`package`]: `.mpak` packages, units compressed in checksummed chunks.
//! - [`Vfs`]: sources of units ([`Package`], [`MemorySource`], later the cook service) mounted
//!   in layers, later layers overriding earlier ones.
//! - [`AssetServer`]: loads objects by link through registered [`Loaders`], hands out
//!   reference-counted [`Handle`]s, and unloads what nothing holds.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cooked;
mod handle;
mod id;
pub mod package;
mod server;
mod vfs;

pub use handle::{AssetRef, Handle, WeakHandle};
pub use id::{Link, ParseUuidError, SourceFile, Uuid};
pub use server::{AssetError, AssetServer, LoadContext, LoadState, Loaders, UpdateStats};
pub use vfs::{MemorySource, Package, Read, SourceError, UNIT_ALIGN, UnitBytes, UnitSource, Vfs};
