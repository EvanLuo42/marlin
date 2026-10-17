//! marlin-asset's platform-independent tests (quick tier, docs/design/0014-testing.md §5): the
//! cooked unit reader, the wire format, identity types and the asset server on in-memory units.
//! Run alone with `cargo test -p marlin-asset --test unit`.

#[path = "../support/mod.rs"]
mod support;

mod cooked;
mod id;
mod reload;
mod server;
mod wire;
