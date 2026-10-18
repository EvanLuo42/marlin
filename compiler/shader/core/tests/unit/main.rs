//! marlin-shaderc-core's tests (quick tier, docs/design/0014-testing.md §5): the core library
//! type-checks, and its functions compute what they should, in the IR interpreter before and
//! after optimization. Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-core --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod library;
mod support;
