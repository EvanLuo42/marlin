//! The `mem-debug` layer: leak table and guard pages, checked by address arithmetic (nothing here
//! faults on purpose).
//!
//! MarlinHeap is not the global allocator here, so only these tests touch the table; they take
//! turns through a lock, since guard pages are a global switch.
#![cfg(feature = "mem-debug")]

use std::alloc::{GlobalAlloc, Layout};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use marlin_mem::MarlinHeap;
use marlin_mem::debug::{
    for_each_live, guard_pages, live_bytes, live_count, set_capture_hook, set_guard_pages,
};
use marlin_mem::vmem::page_size;

static HEAP: MarlinHeap = MarlinHeap::new();
static SERIAL: Mutex<()> = Mutex::new(());

/// Takes the lock; guard pages and the hook are off when the guard drops.
struct Serial(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for Serial {
    fn drop(&mut self) {
        set_guard_pages(false);
        set_capture_hook(None);
    }
}

fn serial() -> Serial {
    Serial(SERIAL.lock().unwrap_or_else(|e| e.into_inner()))
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

fn live() -> HashMap<usize, (usize, Vec<usize>)> {
    let mut all = HashMap::new();
    for_each_live(|addr, size, frames| {
        all.insert(addr, (size, frames.to_vec()));
    });
    all
}

#[test]
fn records_live_allocations() {
    let _serial = serial();
    let (count, bytes) = (live_count(), live_bytes());
    let sizes: Vec<usize> = (0..200).map(|i| 1 + i * 37 % 5000).collect();
    let mut blocks: Vec<(*mut u8, usize)> = sizes
        .iter()
        // SAFETY: non-zero layouts.
        .map(|&size| (unsafe { HEAP.alloc(layout(size, 8)) }, size))
        .collect();
    assert_eq!(live_count(), count + 200);
    assert_eq!(live_bytes(), bytes + sizes.iter().sum::<usize>());
    let table = live();
    for &(p, size) in &blocks {
        assert_eq!(table.get(&p.addr()).map(|e| e.0), Some(size));
    }

    // Reallocation moves the entry along with the block.
    let (p, size) = blocks[0];
    // SAFETY: a live block with its current layout.
    let q = unsafe { HEAP.realloc(p, layout(size, 8), 100_000) };
    blocks[0] = (q, 100_000);
    let table = live();
    assert_eq!(table.get(&q.addr()).map(|e| e.0), Some(100_000));
    if q != p {
        assert!(!table.contains_key(&p.addr()));
    }

    for (p, size) in blocks.drain(..100) {
        // SAFETY: live blocks, freed once with their layouts.
        unsafe { HEAP.dealloc(p, layout(size, 8)) };
    }
    assert_eq!(live_count(), count + 100);
    for (p, size) in blocks {
        // SAFETY: as above.
        unsafe { HEAP.dealloc(p, layout(size, 8)) };
    }
    assert_eq!((live_count(), live_bytes()), (count, bytes));
}

#[test]
fn the_table_grows_past_its_first_size() {
    let _serial = serial();
    let count = live_count();
    // SAFETY: non-zero layouts; each block is freed once below.
    let blocks: Vec<*mut u8> = (0..5000)
        .map(|_| unsafe { HEAP.alloc(layout(24, 8)) })
        .collect();
    assert_eq!(live_count(), count + 5000);
    let table = live();
    assert!(blocks.iter().all(|p| table.contains_key(&p.addr())));
    for p in blocks {
        // SAFETY: as above.
        unsafe { HEAP.dealloc(p, layout(24, 8)) };
    }
    assert_eq!(live_count(), count);
}

fn fake_frames(frames: &mut [usize]) -> usize {
    let fake = [0x1111, 0x2222, 0x3333];
    frames[..3].copy_from_slice(&fake);
    3
}

fn allocating_hook(frames: &mut [usize]) -> usize {
    // Allocates through the heap from inside the hook: recorded, without frames, no recursion.
    let inner = Box::new_in(42u64, MarlinHeap::new());
    frames[0] = *inner as usize;
    1
}

#[test]
fn records_frames_from_the_capture_hook() {
    let _serial = serial();
    set_capture_hook(Some(fake_frames));
    // SAFETY: a non-zero layout, freed below.
    let p = unsafe { HEAP.alloc(layout(64, 8)) };
    set_capture_hook(None);
    // SAFETY: as above.
    let q = unsafe { HEAP.alloc(layout(64, 8)) };
    let table = live();
    assert_eq!(table[&p.addr()].1, [0x1111, 0x2222, 0x3333]);
    assert!(table[&q.addr()].1.is_empty());

    set_capture_hook(Some(allocating_hook));
    let count = live_count();
    // SAFETY: as above.
    let r = unsafe { HEAP.alloc(layout(64, 8)) };
    assert_eq!(live()[&r.addr()].1, [42]);
    // The hook's own block is freed again by the time the allocation is recorded.
    assert_eq!(live_count(), count + 1);
    // SAFETY: live blocks, freed once.
    unsafe {
        for p in [p, q, r] {
            HEAP.dealloc(p, layout(64, 8));
        }
    }
}

#[test]
fn guard_pages_place_blocks_against_an_inaccessible_page() {
    let _serial = serial();
    let page = page_size();
    set_guard_pages(true);
    assert!(guard_pages());
    for size in [1, 8, 16, 100, 4096, 5000, 70_000, 3 << 20] {
        for align in [1, 8, 16] {
            let l = layout(size, align);
            // SAFETY: a non-zero layout; written within its size and freed once.
            unsafe {
                let p = HEAP.alloc_zeroed(l);
                assert!(!p.is_null());
                assert_eq!(p.addr() % align, 0);
                let end = p.addr() + size;
                // The block ends where the guard page starts, up to alignment padding.
                let guard = end.next_multiple_of(page);
                assert!(
                    guard - end < align.max(1),
                    "{l:?}: {} bytes short",
                    guard - end
                );
                if size.is_multiple_of(align) {
                    assert_eq!(end % page, 0, "{l:?}");
                }
                let bytes = std::slice::from_raw_parts_mut(p, size);
                assert!(bytes.iter().all(|&b| b == 0));
                bytes.fill(0xEE);
                assert_eq!(live().get(&p.addr()).map(|e| e.0), Some(size));
                HEAP.dealloc(p, l);
            }
        }
    }
    // Over page alignment starts the block on a page, as close to the guard as alignment allows.
    let l = layout(100, 2 * page);
    // SAFETY: as above.
    unsafe {
        let p = HEAP.alloc(l);
        assert_eq!(p.addr() % (2 * page), 0);
        assert!(p.addr() + 100 <= p.addr().next_multiple_of(page) + page);
        HEAP.dealloc(p, l);
    }
}

#[test]
fn guard_blocks_get_pages_of_their_own_and_move_on_realloc() {
    let _serial = serial();
    let page = page_size();
    set_guard_pages(true);
    // SAFETY: non-zero layouts, written within their sizes, each freed once.
    unsafe {
        let a = HEAP.alloc(layout(32, 16));
        let b = HEAP.alloc(layout(32, 16));
        assert_ne!(a.addr() / page, b.addr() / page);
        a.write_bytes(7, 32);
        let c = HEAP.realloc(a, layout(32, 16), 48);
        assert_ne!(c, a, "guarded blocks always move");
        assert_eq!((c.addr() + 48) % page, 0);
        assert!(std::slice::from_raw_parts(c, 32).iter().all(|&x| x == 7));
        let table = live();
        assert!(!table.contains_key(&a.addr()));
        assert_eq!(table[&c.addr()].0, 48);
        HEAP.dealloc(b, layout(32, 16));
        HEAP.dealloc(c, layout(48, 16));
    }

    // Containers work on guarded blocks (every growth is a move).
    let mut v = Vec::new_in(MarlinHeap::new());
    for i in 0..10_000u32 {
        v.push(i);
    }
    assert!(v.iter().enumerate().all(|(i, &x)| x == i as u32));
}

#[test]
fn blocks_keep_their_kind_across_the_switch() {
    let _serial = serial();
    let count = live_count();
    // SAFETY: non-zero layouts, each freed once.
    unsafe {
        let heap_block = HEAP.alloc(layout(200, 8));
        let other_heap_block = HEAP.alloc(layout(200, 8));
        set_guard_pages(true);
        let guarded = HEAP.alloc(layout(200, 8));
        // With guard pages on, reallocating a heap block moves it into a guarded one.
        let moved = HEAP.realloc(heap_block, layout(200, 8), 208);
        assert_eq!((moved.addr() + 208) % page_size(), 0);
        HEAP.dealloc(moved, layout(208, 8));
        set_guard_pages(false);
        // Each block is freed the way it was allocated.
        assert_eq!((guarded.addr() + 200) % page_size(), 0);
        HEAP.dealloc(guarded, layout(200, 8));
        HEAP.dealloc(other_heap_block, layout(200, 8));
    }
    assert_eq!(live_count(), count);
}

#[test]
fn guard_blocks_survive_many_frees() {
    let _serial = serial();
    set_guard_pages(true);
    // More than the quarantine holds, so the oldest are released along the way.
    for round in 0..3 {
        let blocks: Vec<*mut u8> = (0..800)
            // SAFETY: non-zero layouts, each freed once below.
            .map(|i| unsafe { HEAP.alloc(layout(1 + (i * 13 + round) % 300, 8)) })
            .collect();
        for (i, p) in blocks.into_iter().enumerate() {
            // SAFETY: as above.
            unsafe { HEAP.dealloc(p, layout(1 + (i * 13 + round) % 300, 8)) };
        }
    }
}
