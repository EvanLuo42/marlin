//! marlin-shader's platform-independent tests (quick tier, docs/design/0014-testing.md §5): the
//! reflection types.
//! Run alone with `cargo test -p marlin-shader --test unit`.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod reflect;
