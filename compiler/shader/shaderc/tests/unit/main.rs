//! marlin-shaderc's tests (quick tier, docs/design/0014-testing.md §5): sessions over packages
//! held in memory, compiled for Vulkan, with their reflection and errors. Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc --test unit`; `MARLIN_BLESS=1` rewrites the
//! golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod session;
