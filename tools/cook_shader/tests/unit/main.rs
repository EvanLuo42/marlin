//! marlin-cook-shader's platform-independent tests (quick tier, docs/design/0014-testing.md §5):
//! shader packages through the cook pipeline, and the engine's own package.
//! Run alone with `cargo test -p marlin-cook-shader --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

#[path = "../support/mod.rs"]
mod support;

mod cooker;
