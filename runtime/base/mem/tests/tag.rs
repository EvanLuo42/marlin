//! Memory tags and budgets. The counters are global, so every test uses tags of its own.

use std::alloc::{Allocator, Layout};

use marlin_mem::budget::{budget_remaining, for_each_over_budget};
use marlin_mem::tag::{reset_peak, track_alloc, track_free};
use marlin_mem::{
    Arena, MarlinHeap, MemTag, TagStats, Tagged, budget, is_over_budget, set_budget, tag_stats,
};

#[test]
fn tags_have_names_and_indices() {
    for (i, tag) in MemTag::ALL.into_iter().enumerate() {
        assert_eq!(tag.index(), i);
        assert!(!tag.name().is_empty());
    }
    assert_eq!(MemTag::ALL.len(), MemTag::COUNT);
}

#[test]
fn counts_container_growth_and_release() {
    let tag = MemTag::Audio;
    let base = tag_stats(tag);
    let mut v: Vec<u32, Tagged> = Vec::with_capacity_in(100, Tagged::heap(tag));
    assert_eq!(
        tag_stats(tag),
        TagStats {
            bytes: base.bytes + 400,
            peak_bytes: base.bytes + 400,
            allocations: base.allocations + 1,
        }
    );
    v.extend(0..10_000);
    let grown = tag_stats(tag);
    assert_eq!(grown.bytes, base.bytes + v.capacity() * 4);
    assert_eq!(grown.allocations, base.allocations + 1);
    v.truncate(10);
    v.shrink_to_fit();
    assert_eq!(tag_stats(tag).bytes, base.bytes + 40);
    // The peak remembers the largest size.
    assert!(tag_stats(tag).peak_bytes >= base.bytes + 40_000);
    drop(v);
    let after = tag_stats(tag);
    assert_eq!(
        (after.bytes, after.allocations),
        (base.bytes, base.allocations)
    );
    reset_peak(tag);
    assert_eq!(tag_stats(tag).peak_bytes, after.bytes);
}

#[test]
fn wraps_any_allocator() {
    let tag = MemTag::Animation;
    let arena = Arena::new(1 << 20).unwrap();
    let alloc = Tagged::new(tag, &arena);
    assert_eq!(alloc.tag(), tag);
    let boxed = Box::new_in([1u8; 300], alloc);
    assert_eq!(tag_stats(tag).bytes, 300);
    drop(boxed);
    assert_eq!(tag_stats(tag).bytes, 0);

    // Zero-sized blocks are not counted.
    let heap = Tagged::new(tag, MarlinHeap::new());
    let zst = Layout::from_size_align(0, 8).unwrap();
    let p = heap.allocate(zst).unwrap();
    assert_eq!(tag_stats(tag).allocations, 0);
    // SAFETY: allocated just above with this layout.
    unsafe {
        let q = heap
            .grow(p.cast(), zst, Layout::from_size_align(64, 8).unwrap())
            .unwrap();
        assert_eq!((tag_stats(tag).bytes, tag_stats(tag).allocations), (64, 1));
        heap.deallocate(q.cast(), Layout::from_size_align(64, 8).unwrap());
    }
    assert_eq!(
        tag_stats(tag),
        TagStats {
            bytes: 0,
            peak_bytes: 300,
            allocations: 0
        }
    );
}

#[test]
fn threads_count_concurrently() {
    let tag = MemTag::Network;
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(move || {
                for i in 0..1000 {
                    let v: Vec<u8, Tagged> = Vec::with_capacity_in(1 + i % 64, Tagged::heap(tag));
                    drop(v);
                    track_alloc(tag, 10);
                    track_free(tag, 10);
                }
            });
        }
    });
    let stats = tag_stats(tag);
    assert_eq!((stats.bytes, stats.allocations), (0, 0));
    assert!(stats.peak_bytes >= 64);
}

#[test]
fn budgets_flag_overruns() {
    let tag = MemTag::Physics;
    assert_eq!(budget(tag), None);
    assert!(!is_over_budget(tag));
    assert_eq!(budget_remaining(tag), None);

    set_budget(tag, Some(1000));
    assert_eq!(budget(tag), Some(1000));
    assert_eq!(budget_remaining(tag), Some(1000));
    let small: Vec<u8, Tagged> = Vec::with_capacity_in(1000, Tagged::heap(tag));
    assert!(!is_over_budget(tag), "at the budget is not over it");
    assert_eq!(budget_remaining(tag), Some(0));
    let more: Vec<u8, Tagged> = Vec::with_capacity_in(1, Tagged::heap(tag));
    assert!(is_over_budget(tag));
    let mut over = Vec::new();
    for_each_over_budget(|t, bytes, budget| over.push((t, bytes, budget)));
    assert!(over.contains(&(tag, 1001, 1000)));
    drop(more);
    assert!(!is_over_budget(tag));
    drop(small);

    set_budget(tag, None);
    assert_eq!(budget(tag), None);
}
