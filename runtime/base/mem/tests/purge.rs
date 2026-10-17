//! Delayed decommit of retired pages, through the committed-bytes statistic.
//!
//! MarlinHeap is not the global allocator here, so only these tests move the statistics; they
//! take turns through a lock.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::Mutex;
use std::time::Duration;

use marlin_mem::{DEFAULT_PURGE_DELAY, MarlinHeap, purge, purge_delay, set_purge_delay, stats};

static HEAP: MarlinHeap = MarlinHeap::new();
static SERIAL: Mutex<()> = Mutex::new(());

/// 1000-byte blocks land in the 1024 bin: 64 to a 64 KiB page.
const BLOCK: usize = 1000;
const PAGE: usize = 64 << 10;
/// About ten pages' worth.
const BLOCKS: usize = 640;

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size, 8).unwrap()
}

fn alloc_blocks(size: usize, count: usize) -> Vec<*mut u8> {
    (0..count)
        .map(|i| {
            // SAFETY: a non-zero layout; the block is written within its size.
            unsafe {
                let p = HEAP.alloc(layout(size));
                assert!(!p.is_null());
                p.write_bytes(i as u8, size);
                p
            }
        })
        .collect()
}

fn free_blocks(blocks: Vec<*mut u8>, size: usize) {
    for p in blocks {
        // SAFETY: allocated by `alloc_blocks` with this size, freed once.
        unsafe { HEAP.dealloc(p, layout(size)) };
    }
}

fn committed() -> usize {
    stats().committed_bytes
}

/// Runs `f` on a fresh thread, so it starts with a heap of its own.
fn on_thread(f: impl FnOnce() + Send) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::scope(|s| s.spawn(f).join().unwrap());
    set_purge_delay(Some(DEFAULT_PURGE_DELAY));
}

#[test]
fn delay_round_trips() {
    // Up to clock ticks.
    let close =
        |a: Option<Duration>, b: Duration| a.unwrap().abs_diff(b) < Duration::from_micros(1);
    on_thread(|| {
        assert!(close(purge_delay(), DEFAULT_PURGE_DELAY));
        set_purge_delay(None);
        assert_eq!(purge_delay(), None);
        set_purge_delay(Some(Duration::ZERO));
        assert_eq!(purge_delay(), Some(Duration::ZERO));
        set_purge_delay(Some(Duration::from_millis(250)));
        assert!(close(purge_delay(), Duration::from_millis(250)));
    });
}

#[test]
fn explicit_purge_decommits_retired_pages() {
    on_thread(|| {
        set_purge_delay(None);
        // Keeps the segment alive once the bin's pages empty.
        let anchor = alloc_blocks(16, 1);
        let blocks = alloc_blocks(BLOCK, BLOCKS);
        let full = committed();
        assert!(full >= BLOCKS * BLOCK, "{:?}", stats());
        free_blocks(blocks, BLOCK);
        // Retired, but still committed: purging is off.
        assert_eq!(committed(), full);
        purge();
        let purged = committed();
        // All but the bin's last page go.
        assert!(purged + 8 * PAGE <= full, "{full} -> {purged}");

        // Purged pages are committed again on reuse.
        let again = alloc_blocks(BLOCK, BLOCKS);
        assert!(committed() >= full - PAGE);
        for (i, &p) in again.iter().enumerate() {
            // SAFETY: a live block of `BLOCK` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(p, BLOCK) };
            assert!(bytes.iter().all(|&b| b == i as u8));
        }
        free_blocks(again, BLOCK);
        free_blocks(anchor, 16);
    });
}

#[test]
fn zero_delay_decommits_at_once() {
    on_thread(|| {
        set_purge_delay(Some(Duration::ZERO));
        let anchor = alloc_blocks(16, 1);
        let blocks = alloc_blocks(BLOCK, BLOCKS);
        let full = committed();
        free_blocks(blocks, BLOCK);
        let after = committed();
        assert!(after + 8 * PAGE <= full, "{full} -> {after}");
        free_blocks(anchor, 16);
    });
}

#[test]
fn pages_are_purged_after_the_delay() {
    on_thread(|| {
        set_purge_delay(Some(Duration::from_millis(100)));
        let anchor = alloc_blocks(16, 1);
        let blocks = alloc_blocks(BLOCK, BLOCKS);
        let full = committed();
        free_blocks(blocks, BLOCK);
        // Within the delay the memory stays, ready for reuse.
        assert_eq!(committed(), full);

        std::thread::sleep(Duration::from_millis(150));
        // The next retire on this thread purges the expired pages: three 2 KiB-bin pages, two of
        // them retired.
        let other = alloc_blocks(2000, 96);
        free_blocks(other, 2000);
        let after = committed();
        assert!(after + 6 * PAGE <= full, "{full} -> {after}");
        free_blocks(anchor, 16);
    });
}

#[test]
fn purge_releases_cached_segments() {
    on_thread(|| {
        // A huge allocation's segment goes to the cache when freed.
        let huge = alloc_blocks(1 << 20, 1);
        free_blocks(huge, 1 << 20);
        assert!(stats().cached_segments > 0);
        let reserved = stats().reserved_bytes;
        purge();
        assert_eq!(stats().cached_segments, 0);
        assert!(stats().reserved_bytes < reserved);
    });
}
