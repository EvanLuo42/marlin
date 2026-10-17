//! marlin-masset's tests (quick tier, docs/design/0014-testing.md §5): parsing and writing
//! documents, binding them to reflected types, and the indexing fast path. The ignored
//! `bench_extract_and_parse` is a benchmark (`--ignored --nocapture`, release).
//! Run alone with `cargo test -p marlin-masset --test unit`.

mod bind;
mod extract;
mod syntax;
