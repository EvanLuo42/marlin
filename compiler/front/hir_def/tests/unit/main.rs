//! marlin-front-hir-def's tests (quick tier, docs/design/0014-testing.md §5): item trees,
//! package def maps and their diagnostics (golden files), C# name lookup and accessibility,
//! bodies and signatures lowered to HIR (golden files), local scopes, incremental reuse across
//! edits, and robustness on broken input (`MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`, 1 GiB
//! memory cap). Platform-independent.
//! Run alone with `cargo test -p marlin-front-hir-def --test unit`; `MARLIN_BLESS=1` rewrites
//! the golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod fixture;

mod bodies;
mod def_map;
mod incremental;
mod resolve;
mod robustness;
