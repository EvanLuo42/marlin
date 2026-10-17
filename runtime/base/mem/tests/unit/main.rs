//! marlin-mem's tests (quick tier, docs/design/0014-testing.md §5): pages, arenas, pools, tags
//! and `MarlinHeap` through its API, with the system allocator still global. No hardware needed.
//! Tests that need `MarlinHeap` as the global allocator or move its global statistics are in the
//! `isolated_*` binaries. Run alone with `cargo test -p marlin-mem --test unit`.

mod alloc_api;
mod arena;
mod ext;
mod heap;
mod pool;
mod tag;
mod vmem;
