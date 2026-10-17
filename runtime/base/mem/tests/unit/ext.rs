//! Large-page arenas, mirrored rings and the system memory query.

#[test]
fn system_memory_is_plausible() {
    let info = marlin_mem::system_memory().unwrap();
    assert!(info.total_physical >= info.available_physical);
    assert!(info.process_resident > 0);
}

#[cfg(marlin_ext = "vmem_alias")]
#[test]
fn mirrored_ring_wraps_contiguously() {
    use marlin_mem::ext::vmem_alias::{MirroredRing, VmemAlias};

    let alias = VmemAlias::get().unwrap();
    let ring = MirroredRing::new(alias, 1000).unwrap();
    let cap = ring.capacity();
    assert!(cap >= 1000 && cap.is_multiple_of(alias.granularity()));
    // SAFETY: no other slice of the ring is alive.
    let tail = unsafe { ring.slice_mut(cap - 3, 6) };
    tail.copy_from_slice(b"abcdef");
    // SAFETY: as above.
    let head = unsafe { ring.slice_mut(0, 3) };
    assert_eq!(head, b"def");
    // Offsets wrap.
    // SAFETY: as above.
    assert_eq!(unsafe { ring.slice_mut(cap * 5, 3) }, b"def");
    // SAFETY: nothing relies on the contents afterwards.
    unsafe { ring.discard(alias) }.unwrap();
}

#[cfg(marlin_ext = "large_pages")]
#[test]
fn large_page_arena_when_available() {
    use marlin_mem::ext::large_pages::{self, LargePages};

    // Most machines grant no large pages to a test process; check the arena only where they do.
    let Some(lp) = LargePages::get() else {
        marlin_test_support::skip!("no large pages granted to this process");
    };
    let Ok(mut arena) = large_pages::arena(lp, 1) else {
        marlin_test_support::skip!("no large pages free for an arena");
    };
    assert_eq!(arena.capacity(), lp.page_size());
    assert_eq!(arena.committed(), lp.page_size());
    let value = arena.alloc(42u64);
    assert_eq!(*value, 42);
    // Trimming never decommits large pages.
    arena.reset();
    arena.trim(Some(0));
    assert_eq!(arena.committed(), lp.page_size());
}
