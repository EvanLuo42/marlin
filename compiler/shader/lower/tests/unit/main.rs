//! marlin-shaderc-lower's tests (quick tier, docs/design/0014-testing.md §5): Marlin shader
//! source through type checking and lowering to IR that verifies, run in the IR interpreter
//! and checked against what the source means.
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-lower --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod support;

mod basics;
mod closures;
mod compute;
mod generics;
mod members;
mod robustness;
