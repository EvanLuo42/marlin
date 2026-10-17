//! MarlinHeap through its public API, without registering it as the global allocator.

use std::alloc::{GlobalAlloc, Layout};

use marlin_mem::{MarlinHeap, good_size};

#[test]
fn alloc_write_dealloc_across_classes() {
    let heap = MarlinHeap::new();
    for size in [1, 8, 24, 100, 4096, 9000, 65536, 70_000, 5 << 20] {
        for align in [1, 8, 16, 64, 4096] {
            let layout = Layout::from_size_align(size, align).unwrap();
            // SAFETY: the layout is non-zero; the block is used within its size and freed once.
            unsafe {
                let p = heap.alloc_zeroed(layout);
                assert!(!p.is_null(), "{layout:?}");
                assert_eq!(p.addr() % align, 0, "{layout:?}");
                assert_eq!(p.add(size - 1).read(), 0);
                p.write(1);
                p.add(size - 1).write(2);
                heap.dealloc(p, layout);
            }
        }
    }
}

#[test]
fn refuses_alignment_above_half_a_segment() {
    let heap = MarlinHeap::new();
    let layout = Layout::from_size_align(8, 4 << 20).unwrap();
    // SAFETY: a valid layout; a null result is not dereferenced.
    assert!(unsafe { heap.alloc(layout) }.is_null());
}

#[test]
fn size_classes_cover_every_size_tightly() {
    let mut previous = 0;
    for size in 1..=(64 << 10) {
        let class = good_size(size);
        assert!(class >= size, "size {size}");
        assert!(class >= previous, "classes must not shrink: {size}");
        if class != previous {
            // A new class starts right after the previous one ends.
            assert!(previous < size, "size {size}");
        }
        if size > 64 {
            // At most 25% internal waste, and 16-byte multiples for alignment.
            assert!(class * 4 <= size * 5 + 64, "size {size} -> {class}");
            assert_eq!(class % 16, 0, "size {size}");
        } else {
            assert_eq!(class, size.div_ceil(8) * 8);
        }
        previous = class;
    }
    assert_eq!(good_size(0), 8);
    assert_eq!(good_size((64 << 10) + 1) % 4096, 0);
}

#[test]
fn reallocating_within_a_class_keeps_the_block() {
    let heap = MarlinHeap::new();
    let layout = Layout::from_size_align(100, 8).unwrap();
    // SAFETY: the block is resized within its class and freed once with its final layout.
    unsafe {
        let p = heap.alloc(layout);
        let grown = heap.realloc(p, layout, good_size(100));
        assert_eq!(p, grown, "growing into the class's slack must not move");
        heap.dealloc(grown, Layout::from_size_align(good_size(100), 8).unwrap());
    }
}
