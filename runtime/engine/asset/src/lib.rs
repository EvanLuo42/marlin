//! Assets at runtime: identity, cooked units, packages, mounting and loading
//! (`docs/design/0003-asset-pipeline.md` §7–§8).
//!
//! - Identity: [`Uuid`], [`Link`], [`SourceFile`].
//! - [`cooked`]: the cooked unit format the cooker writes and the runtime reads in place.
//! - [`package`]: `.mpak` packages, units compressed in checksummed chunks.
//! - [`Vfs`]: sources of units ([`Package`], [`MemorySource`], [`DevSource`]: the cook service) mounted
//!   in layers, later layers overriding earlier ones.
//! - [`wire`]: framing of the asset tools' TCP protocols.
//! - [`AssetServer`]: loads objects by link through registered [`Loaders`], hands out
//!   reference-counted [`Handle`]s, and unloads what nothing holds.
//! - [`BlobStreamer`]: reads streamed bulk blobs one by one when wanted.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cooked;
pub mod dev;
mod handle;
mod id;
mod in_place;
mod le;
pub mod package;
mod server;
pub mod stream;
mod vfs;
pub mod wire;

pub use dev::DevSource;
pub use handle::{AssetRef, Handle, WeakHandle};
pub use id::{Link, ParseUuidError, SourceFile, Uuid, name_hash};
pub use in_place::{CookedAsset, InPlace};
pub use server::{AssetError, AssetServer, BlobInfo, LoadContext, LoadState, Loaders, UpdateStats};
pub use stream::{BlobRequest, BlobStreamer, StreamStats};
pub use vfs::{MemorySource, Package, Read, SourceError, UNIT_ALIGN, UnitBytes, UnitSource, Vfs};
