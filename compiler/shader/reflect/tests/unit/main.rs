//! marlin-shaderc-reflect's tests (quick tier, docs/design/0014-testing.md §5): reflection
//! built from IR and front-end facts. Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-reflect --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod reflect;
