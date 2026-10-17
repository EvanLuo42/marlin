//! The `mem-debug` leak table with `MarlinHeap` as the global allocator, recording stacks through
//! the PAL's walker (quick tier, docs/design/0014-testing.md §5). Its own binary, since the global
//! allocator is per binary. Needs the `mem-debug` feature; no hardware.
//! Run alone with `cargo test -p marlin-mem --features mem-debug --test isolated_debug_global`.
#![cfg(feature = "mem-debug")]

#[test]
fn leak_table_records_stacks_with_the_pal_walker() {
    use marlin_mem::debug;

    #[global_allocator]
    static HEAP: marlin_mem::MarlinHeap = marlin_mem::MarlinHeap::new();

    debug::set_capture_hook(Some(debug::capture_stack));
    let block = std::hint::black_box(vec![7u8; 4321]);
    debug::set_capture_hook(None);
    let addr = block.as_ptr().addr();
    let mut frames = 0;
    debug::for_each_live(|a, size, stack| {
        if a == addr {
            assert_eq!(size, 4321);
            frames = stack.len();
        }
    });
    assert!(frames > 2, "{frames}");
    drop(block);
}
