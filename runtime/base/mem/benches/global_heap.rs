//! `MarlinHeap` against the system allocator: 1024 small blocks allocated, then freed, per
//! iteration. The file runs on `MarlinHeap`, as a game would, so the harness's own allocations go
//! through it too.
//! `cargo xtask test --tier bench -p marlin-mem`, or
//! `cargo bench -p marlin-mem --bench global_heap -- [filter]`.
#![allow(clippy::undocumented_unsafe_blocks)]

use std::alloc::{GlobalAlloc, Layout, System};

use marlin_bench::Bench;
use marlin_mem::MarlinHeap;

#[global_allocator]
static HEAP: MarlinHeap = MarlinHeap::new();

fn alloc_free() {
    fn run<A: GlobalAlloc>(name: &str, a: &A) {
        let layouts: Vec<Layout> = (0..1024)
            .map(|i| Layout::from_size_align(16 + (i * 37) % 512, 8).unwrap())
            .collect();
        let mut ptrs = vec![std::ptr::null_mut(); 1024];
        Bench::new(format!("{name}/alloc_free"))
            .throughput_items(layouts.len() as u64, "alloc")
            .run(|| {
                for (p, &l) in ptrs.iter_mut().zip(&layouts) {
                    *p = unsafe { a.alloc(l) };
                }
                for (&p, &l) in ptrs.iter().zip(&layouts) {
                    unsafe { a.dealloc(p, l) };
                }
            });
    }
    run("system", &System);
    run("marlin", &HEAP);
}

marlin_bench::main!(alloc_free);
