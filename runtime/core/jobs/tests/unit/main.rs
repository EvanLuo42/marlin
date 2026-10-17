//! marlin-jobs's tests (quick tier, docs/design/0014-testing.md §5): scopes, handles, graphs,
//! cancellation, the fiber pool, the main thread as a worker, and fiber-aware sync. On the host's
//! PAL. Run alone with `cargo test -p marlin-jobs --test unit`; the ignored throughput benchmark
//! is in `jobs`.

#[path = "../support/mod.rs"]
mod support;

mod cancel;
mod fibers;
mod graph;
mod handles;
mod jobs;
mod main_thread;
mod sync;

use marlin_std::mem::MarlinHeap;

/// Jobs that allocate, park, resume on another worker and allocate again exercise the allocator's
/// thread-local heaps under fiber migration.
#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();
