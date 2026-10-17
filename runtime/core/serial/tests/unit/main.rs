//! marlin-serial's tests (quick tier, docs/design/0014-testing.md §5): the binary format, headers,
//! every reflected kind, migration, cooked data and streams. Platform-independent.
//! Run alone with `cargo test -p marlin-serial --test unit`.

mod cooked;
mod header;
mod kinds;
mod migrate;
mod serial;
mod stream;
