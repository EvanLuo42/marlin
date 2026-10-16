//! Linear and frame arenas.

use std::alloc::Layout;
use std::sync::Barrier;

use marlin_mem::arena::COMMIT_GRANULE;
use marlin_mem::{Arena, FrameArenas};

#[test]
fn allocates_aligned_values() {
    let arena = Arena::new(1 << 20).unwrap();
    assert!(arena.capacity() >= 1 << 20);
    assert_eq!((arena.used(), arena.committed()), (0, 0));
    let byte = arena.alloc(7u8);
    let word = arena.alloc(0x1122_3344_5566_7788u64);
    let page = arena.alloc_layout(Layout::from_size_align(16, 4096).unwrap());
    assert_eq!((*byte, *word), (7, 0x1122_3344_5566_7788));
    assert_eq!(std::ptr::from_mut(word) as usize % 8, 0);
    assert_eq!(page.as_ptr() as usize % 4096, 0);
    *byte = 9;
    assert_eq!(*byte, 9);
    assert_eq!(arena.committed(), COMMIT_GRANULE);
    // Zero-sized values take no room.
    let used = arena.used();
    arena.alloc(());
    assert_eq!(arena.used(), used);
}

#[test]
fn allocates_slices_and_strings() {
    let arena = Arena::new(1 << 20).unwrap();
    let copy = arena.alloc_slice_copy(&[1u32, 2, 3]);
    copy[1] = 20;
    assert_eq!(copy, [1, 20, 3]);
    let squares = arena.alloc_slice_fill_with(5, |i| (i * i) as u16);
    assert_eq!(squares, [0, 1, 4, 9, 16]);
    let s = arena.alloc_str("frame arena ✓");
    assert_eq!(s, "frame arena ✓");
    let uninit = arena.alloc_uninit_slice::<u64>(3);
    assert_eq!(uninit.len(), 3);
    assert!(arena.alloc_slice_copy::<u8>(&[]).is_empty());
}

#[test]
fn commits_as_it_grows() {
    let mut arena = Arena::new(64 << 20).unwrap();
    let block = Layout::from_size_align(COMMIT_GRANULE / 2, 16).unwrap();
    for i in 1..=8 {
        let p = arena.alloc_layout(block);
        // Every committed byte is writable.
        // SAFETY: a fresh block of `block.size()` bytes.
        unsafe { p.as_ptr().write_bytes(i as u8, block.size()) };
        assert_eq!(
            arena.committed(),
            (i * block.size()).next_multiple_of(COMMIT_GRANULE)
        );
    }
    // Resets keep the memory.
    let committed = arena.committed();
    arena.reset();
    assert_eq!((arena.used(), arena.committed()), (0, committed));
}

#[test]
fn runs_out_at_its_capacity() {
    let arena = Arena::new(1 << 16).unwrap();
    let all = Layout::from_size_align(arena.capacity(), 1).unwrap();
    assert!(arena.try_alloc_layout(all).is_some());
    assert!(arena.try_alloc_layout(Layout::new::<u8>()).is_none());
    assert!(
        arena
            .try_alloc_layout(Layout::from_size_align(usize::MAX / 2, 1).unwrap())
            .is_none()
    );
}

#[test]
#[should_panic(expected = "arena exhausted")]
fn alloc_panics_when_exhausted() {
    let arena = Arena::new(1 << 16).unwrap();
    arena.alloc_uninit_slice::<u8>(arena.capacity() + 1);
}

#[test]
fn threads_allocate_disjoint_blocks() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 2000;
    let arena = Arena::new(256 << 20).unwrap();
    let barrier = Barrier::new(THREADS);
    let blocks: Vec<Vec<(usize, usize)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let (arena, barrier) = (&arena, &barrier);
                s.spawn(move || {
                    barrier.wait();
                    (0..PER_THREAD)
                        .map(|i| {
                            let len = 1 + (i * 37 + t * 11) % 300;
                            let block = arena.alloc_slice_fill_with(len, |_| t as u8);
                            (block.as_ptr() as usize, len)
                        })
                        .collect()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut all: Vec<(usize, usize, usize)> = blocks
        .iter()
        .enumerate()
        .flat_map(|(t, b)| b.iter().map(move |&(p, l)| (p, l, t)))
        .collect();
    all.sort_unstable();
    for pair in all.windows(2) {
        assert!(pair[0].0 + pair[0].1 <= pair[1].0, "overlapping blocks");
    }
    // Each block still holds its thread's byte: nothing was handed out twice.
    for &(p, len, t) in &all {
        // SAFETY: each block is live arena memory of `len` bytes, no longer borrowed elsewhere.
        let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, len) };
        assert!(bytes.iter().all(|&b| b == t as u8));
    }
}

#[test]
fn collections_grow_in_place() {
    let arena = Arena::new(64 << 20).unwrap();
    let mut v: Vec<u64, &Arena> = Vec::new_in(&arena);
    for i in 0..100_000u64 {
        v.push(i);
    }
    // The last allocation grows where it is: no doubling copies left behind.
    assert!(
        arena.used() <= v.capacity() * 8 + 64,
        "{} used",
        arena.used()
    );
    assert!(v.iter().enumerate().all(|(i, &x)| x == i as u64));

    // Interleaved growth falls back to copying.
    let mut a: Vec<u32, &Arena> = Vec::new_in(&arena);
    let mut b: Vec<u32, &Arena> = Vec::new_in(&arena);
    for i in 0..1000 {
        a.push(i);
        b.push(i * 2);
    }
    assert!(a.iter().zip(&b).all(|(&x, &y)| y == x * 2));
    // Dropping the last allocation hands its room back.
    let used = arena.used();
    let tail: Vec<u8, &Arena> = Vec::with_capacity_in(4096, &arena);
    assert!(arena.used() >= used + 4096);
    drop(tail);
    assert_eq!(arena.used(), used);
}

#[test]
fn scopes_and_checkpoints_rewind() {
    let mut arena = Arena::new(1 << 20).unwrap();
    arena.alloc(1u32);
    let before = arena.used();
    let sum = arena.scope(|a| {
        let values = a.alloc_slice_fill_with(1000, |i| i as u32);
        values.iter().sum::<u32>()
    });
    assert_eq!(sum, 499_500);
    assert_eq!(arena.used(), before);

    let mark = arena.checkpoint();
    arena.alloc([0u8; 100]);
    assert!(arena.used() > before);
    // SAFETY: the array above is not used again.
    unsafe { arena.rewind(mark) };
    assert_eq!(arena.used(), before);
}

#[test]
fn trims_back_to_its_peak_or_less() {
    let mut arena = Arena::new(64 << 20).unwrap();
    arena.alloc_uninit_slice::<u8>(10 << 20);
    arena.reset();
    let committed = arena.committed();
    assert!(committed >= 10 << 20);
    // The peak since the last trim stays committed.
    arena.trim(None);
    assert_eq!(arena.committed(), committed);
    // Another quiet trim gives it back.
    arena.trim(None);
    assert_eq!(arena.committed(), 0);
    arena.alloc_uninit_slice::<u8>(3 * COMMIT_GRANULE);
    arena.trim(Some(0));
    // Never below what is in use.
    assert_eq!(arena.committed(), 3 * COMMIT_GRANULE);
    // Committed again after a trim, the memory is usable.
    arena.reset();
    let block = arena.alloc_slice_fill_with(5 * COMMIT_GRANULE, |_| 0xabu8);
    assert!(block.iter().all(|&b| b == 0xab));
}

#[test]
fn frame_arenas_rotate() {
    let mut frames = FrameArenas::<3>::new(1 << 20).unwrap();
    for frame in 0..9u64 {
        // SAFETY: nothing from three frames ago is used.
        let arena = unsafe { frames.begin(frame) };
        assert_eq!(arena.used(), 0, "frame {frame}");
        let value = arena.alloc(frame);
        assert_eq!(*value, frame);
        assert!(std::ptr::eq(arena, frames.get(frame)));
        assert!(!std::ptr::eq(frames.get(frame), frames.get(frame + 1)));
        assert!(std::ptr::eq(frames.get(frame), frames.get(frame + 3)));
    }
    assert_eq!(frames.committed(), 3 * COMMIT_GRANULE);
    frames.reset();
    frames.trim(Some(0));
    assert_eq!(frames.committed(), 0);
}
