//! Loopback throughput: pipelined small requests and bulk transfer, in both receive modes.
//! `cargo xtask test --tier bench -p marlin-net`, or
//! `cargo bench -p marlin-net --bench loopback -- [filter]`.
//!
//! Only the Windows backend exists so far; elsewhere this is empty.

#[cfg(marlin_platform = "windows")]
#[path = "../../tests/support/mod.rs"]
mod support;
#[cfg(marlin_platform = "windows")]
#[path = "platform/windows.rs"]
mod windows;

#[cfg(marlin_platform = "windows")]
marlin_bench::main!(windows::requests_per_second, windows::bulk_transfer);

#[cfg(not(marlin_platform = "windows"))]
fn main() {}
