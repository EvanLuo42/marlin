//! marlin-shaderc-hir-ty's tests (quick tier, docs/design/0014-testing.md §5): built-ins of a
//! stand-in core library, type lowering of signatures, body type inference, type diagnostics
//! (golden files), and robustness on broken input (`MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`,
//! 1 GiB memory cap).
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-hir-ty --test unit`; `MARLIN_BLESS=1` rewrites
//! the golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod support;

mod bodies;
mod robustness;
mod types;
