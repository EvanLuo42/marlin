//! MarlinHeap as an `Allocator`, without registering it as the global allocator.

use std::alloc::{Allocator, Layout};
use std::ptr::NonNull;

use marlin_mem::{MarlinHeap, good_size};

const HEAP: MarlinHeap = MarlinHeap::new();

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn collections_allocate_from_the_heap() {
    let mut v: Vec<u64, MarlinHeap> = Vec::new_in(HEAP);
    for i in 0..100_000u64 {
        v.push(i);
    }
    assert!(v.iter().enumerate().all(|(i, &x)| x == i as u64));
    v.truncate(10);
    v.shrink_to_fit();
    assert_eq!(v, (0..10).collect::<Vec<_>>());

    let b = Box::new_in([7u32; 1000], HEAP);
    assert!(b.iter().all(|&x| x == 7));
    // Zero-sized values allocate nothing.
    let unit = Box::new_in((), HEAP);
    drop(unit);
    let mut strings: Vec<Vec<u8, MarlinHeap>, MarlinHeap> = Vec::new_in(HEAP);
    for i in 0..1000 {
        let mut s = Vec::with_capacity_in(i % 100, HEAP);
        s.extend((0..i % 100).map(|j| j as u8));
        strings.push(s);
    }
    assert!(strings.iter().enumerate().all(|(i, s)| s.len() == i % 100));
}

#[test]
fn grows_and_shrinks_in_place_within_a_class() {
    let old = layout(100, 8);
    let p = HEAP.allocate(old).unwrap().cast::<u8>();
    // SAFETY: each resize passes the block's current layout; it is freed once at the end.
    unsafe {
        p.as_ptr().write_bytes(0xAB, 100);
        let grown = HEAP.grow(p, old, layout(good_size(100), 8)).unwrap();
        assert_eq!(
            grown.cast::<u8>(),
            p,
            "growing into the class's slack must not move"
        );
        let shrunk = HEAP
            .shrink(grown.cast(), layout(good_size(100), 8), layout(64, 8))
            .unwrap();
        assert_eq!(shrunk.cast::<u8>(), p, "a small shrink must not move");
        let bytes = std::slice::from_raw_parts(p.as_ptr(), 64);
        assert!(bytes.iter().all(|&b| b == 0xAB));
        // Far past the class it moves, keeping the contents.
        let moved = HEAP.grow(p, layout(64, 8), layout(10_000, 8)).unwrap();
        let bytes = std::slice::from_raw_parts(moved.cast::<u8>().as_ptr(), 64);
        assert!(bytes.iter().all(|&b| b == 0xAB));
        HEAP.deallocate(moved.cast(), layout(10_000, 8));
    }
}

#[test]
fn grow_zeroed_clears_the_new_bytes() {
    for (from, to) in [(16, 24), (64, 4096), (5000, 200_000), (100_000, 300_000)] {
        let p = HEAP.allocate(layout(from, 8)).unwrap().cast::<u8>();
        // SAFETY: the block is resized with its current layout and freed once.
        unsafe {
            p.as_ptr().write_bytes(0xFF, from);
            let q = HEAP
                .grow_zeroed(p, layout(from, 8), layout(to, 8))
                .unwrap()
                .cast::<u8>();
            let bytes = std::slice::from_raw_parts(q.as_ptr(), to);
            assert!(bytes[..from].iter().all(|&b| b == 0xFF), "{from} -> {to}");
            assert!(bytes[from..].iter().all(|&b| b == 0), "{from} -> {to}");
            HEAP.deallocate(q, layout(to, 8));
        }
    }
}

#[test]
fn honors_a_larger_alignment_when_resizing() {
    let p = HEAP.allocate(layout(24, 8)).unwrap().cast::<u8>();
    // SAFETY: as above.
    unsafe {
        p.as_ptr().write_bytes(3, 24);
        for (size, align) in [(2000, 64), (3000, 4096), (100, 256), (1 << 20, 1 << 16)] {
            let q = HEAP.allocate(layout(size, align)).unwrap().cast::<u8>();
            assert_eq!(q.addr().get() % align, 0);
            HEAP.deallocate(q, layout(size, align));
        }
        let q = HEAP.grow(p, layout(24, 8), layout(5000, 512)).unwrap();
        assert_eq!(q.cast::<u8>().addr().get() % 512, 0);
        let bytes = std::slice::from_raw_parts(q.cast::<u8>().as_ptr(), 24);
        assert!(bytes.iter().all(|&b| b == 3));
        HEAP.deallocate(q.cast(), layout(5000, 512));
    }
}

#[test]
fn zero_sized_blocks_are_dangling() {
    let zst = layout(0, 64);
    let p = HEAP.allocate(zst).unwrap();
    assert_eq!(p.len(), 0);
    assert_eq!(p.cast::<u8>().addr().get() % 64, 0);
    // SAFETY: resizing from and to zero, with the current layouts.
    unsafe {
        let q = HEAP.grow(p.cast(), zst, layout(40, 64)).unwrap();
        q.cast::<u8>().as_ptr().write_bytes(1, 40);
        let r = HEAP.shrink(q.cast(), layout(40, 64), zst).unwrap();
        assert_eq!(r.len(), 0);
        HEAP.deallocate(r.cast(), zst);
    }
}

#[test]
fn refuses_alignment_above_half_a_segment() {
    assert!(HEAP.allocate(layout(8, 4 << 20)).is_err());
}

#[test]
fn shared_references_allocate_too() {
    let heap = &HEAP;
    let v: Vec<u8, &MarlinHeap> = vec![1, 2, 3].into_iter().collect_in(heap);
    assert_eq!(*v, [1, 2, 3]);
    let block: NonNull<[u8]> = heap.allocate(layout(10, 1)).unwrap();
    // SAFETY: allocated just above with this layout.
    unsafe { heap.deallocate(block.cast(), layout(10, 1)) };
}

trait CollectIn<T> {
    fn collect_in<A: Allocator>(self, alloc: A) -> Vec<T, A>;
}

impl<T, I: Iterator<Item = T>> CollectIn<T> for I {
    fn collect_in<A: Allocator>(self, alloc: A) -> Vec<T, A> {
        let mut v = Vec::new_in(alloc);
        v.extend(self);
        v
    }
}
