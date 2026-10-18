//! marlin-shaderc-opt's tests (quick tier, docs/design/0014-testing.md §5): each pass and the
//! pipeline on hand-written IR (golden files of what they make), and differential tests
//! (0005 §4.6): random structured functions, run in the IR interpreter on random inputs before
//! and after each pass (`MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`, 1 GiB memory cap).
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-opt --test unit`; `MARLIN_BLESS=1` rewrites the
//! golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod autodiff;
mod differential;
mod passes;

mod uniformity;
