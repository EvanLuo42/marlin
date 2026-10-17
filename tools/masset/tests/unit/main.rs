//! marlin-masset's tests (quick tier, docs/design/0014-testing.md §5): parsing and writing
//! documents, binding them to reflected types, and the indexing fast path.
//! Run alone with `cargo test -p marlin-masset --test unit`.

mod bind;
mod extract;
mod syntax;
