//! marlin-front-syntax's tests (quick tier, docs/design/0014-testing.md §5): the lexer, tree
//! snapshots of the superset grammar, AST accessors, and robustness against any input (seeded
//! random inputs, `MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`; replay of the inputs the
//! `front_syntax_parse` fuzz target found). Platform-independent. The whole binary runs under a
//! 1 GiB memory cap.
//! Run alone with `cargo test -p marlin-front-syntax --test unit`; `MARLIN_BLESS=1` rewrites the
//! tree snapshots.

use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

mod ast;
mod lexer;
mod robustness;
mod snapshots;
