//! marlin-gpu-test-support's tests (quick tier, docs/design/0014-testing.md §5): every shared
//! test shader compiles, without a GPU. Platform-independent.
//! Run alone with `cargo test -p marlin-gpu-test-support --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod shaders;
