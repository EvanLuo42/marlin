//! marlin-shaderc-ir's tests (quick tier, docs/design/0014-testing.md §5): the text format
//! round-trips (fixtures and golden files), the builder and function editing, the verifier (a
//! golden file of broken modules), and the parser and verifier on
//! broken input (`MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`, 1 GiB memory cap).
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-ir --test unit`; `MARLIN_BLESS=1` rewrites the
//! golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod builder;
mod numbers;
mod robustness;
mod text;
mod verify;
