//! marlin-lang-query's tests (quick tier, docs/design/0014-testing.md §5): memoization and
//! red-green reuse, backdating, durability, a model test against direct evaluation
//! (`MARLIN_TEST_SEED` / `MARLIN_TEST_SCALE`), threads sharing work, cooperative cancellation,
//! and cycles. Platform-independent. Thread tests are bounded by `with_timeout`.
//! Run alone with `cargo test -p marlin-lang-query --test unit`.

mod cancellation;
mod cycles;
mod incremental;
mod model;
mod threads;
