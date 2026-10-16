//! Asset handles, package mounting and streaming.
//!
//! So far asset identity ([`Uuid`], [`Link`], [`SourceFile`]) and the [`cooked`] unit format;
//! loading follows in phase P4 of `docs/design/0003-asset-pipeline.md`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cooked;
mod id;

pub use id::{Link, ParseUuidError, SourceFile, Uuid};
