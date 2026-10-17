//! marlin-containers's tests (quick tier, docs/design/0014-testing.md §5): each container against
//! a std model, drops tracked with `marlin_test_support::Tracker`. Platform-independent.
//! Run alone with `cargo test -p marlin-containers --test unit`.

mod alloc;
mod array_vec;
mod bit_set;
mod deque;
mod hash;
mod index_map;
mod secondary_map;
mod slot_map;
mod small_vec;
mod sparse_set;
mod spsc;
mod vec_ops;
