//! marlin-shaderc-backend-spirv's tests (quick tier, docs/design/0014-testing.md §5): IR
//! fixtures written as SPIR-V (golden disassemblies), and the SPIR-V numbers.
//! Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-backend-spirv --test unit`; `MARLIN_BLESS=1`
//! rewrites the golden files.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod write;
