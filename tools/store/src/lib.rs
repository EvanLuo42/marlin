//! `marlin-store`: the shared derived-data cache (`docs/design/0003-asset-pipeline.md` §5).
//!
//! - [`Store`]: content-addressed entries (cooked units, manifests) in append-only pack files
//!   (`marlin_cook_core::LocalDdc`), and [`Refs`], named mutable pointers to keys.
//! - [`protocol`]: the requests (`GET`, `HAS`, `PUT`, `BATCH`, `REF_GET`, `REF_SET`, `STATS`)
//!   over `marlin_asset::wire` frames.
//! - [`Server`]: a pipelined framed TCP server on marlin-net's completion queue; `marlin-cookd`
//!   runs on it too.
//! - [`StoreClient`], and [`RemoteDdc`], the store as a `Ddc` for the cooker.

mod client;
pub mod protocol;
mod server;
mod store;

pub use client::{ClientError, Connection, RemoteDdc, StoreClient};
pub use server::{ConnId, Outbox, Server, ServerError, Service};
pub use store::{Refs, Store};
