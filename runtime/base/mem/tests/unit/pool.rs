//! Fixed-size pools.

use std::alloc::{Allocator, Layout};
use std::collections::HashSet;
use std::ptr::NonNull;
use std::sync::Barrier;

use marlin_mem::pool::COMMIT_GRANULE;
use marlin_mem::{Pool, TypedPool};
use marlin_test_support::{DropCounter, DropToken};

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn rounds_blocks_up_and_commits_lazily() {
    let pool = Pool::new(layout(3, 1), 10).unwrap();
    assert_eq!(pool.block_layout(), layout(8, 8));
    assert!(pool.capacity() >= 10);
    assert_eq!((pool.len(), pool.committed()), (0, 0));
    let p = pool.alloc().unwrap();
    assert_eq!(pool.committed(), COMMIT_GRANULE.min(pool.capacity() * 8));
    assert_eq!(pool.len(), 1);
    assert!(pool.contains(p.as_ptr()));
    assert!(!pool.contains(p.as_ptr().wrapping_add(1)));

    let aligned = Pool::new(layout(100, 64), 10).unwrap();
    assert_eq!(aligned.block_layout(), layout(128, 64));
    let big = Pool::new(layout(16, 1 << 20), 4).unwrap();
    for _ in 0..4 {
        assert_eq!(big.alloc().unwrap().addr().get() % (1 << 20), 0);
    }
    // SAFETY: allocated above and not used again.
    unsafe { pool.free(p) };
}

#[test]
fn reuses_freed_blocks_first() {
    let pool = Pool::new(layout(48, 16), 1000).unwrap();
    let a = pool.alloc().unwrap();
    let b = pool.alloc().unwrap();
    assert_ne!(a, b);
    assert_eq!(b.addr().get() - a.addr().get(), 48);
    // SAFETY: each block is freed once and not used afterwards.
    unsafe {
        pool.free(a);
        assert_eq!(pool.alloc(), Some(a));
        pool.free(b);
        pool.free(a);
    }
    assert_eq!(pool.alloc(), Some(a));
    assert_eq!(pool.alloc(), Some(b));
    assert_eq!(pool.len(), 2);
}

#[test]
fn runs_out_at_its_capacity() {
    let pool = Pool::new(layout(4096, 8), 50).unwrap();
    let blocks: Vec<_> = std::iter::from_fn(|| pool.alloc()).collect();
    assert_eq!(blocks.len(), pool.capacity());
    assert_eq!(pool.len(), pool.capacity());
    let distinct: HashSet<_> = blocks.iter().collect();
    assert_eq!(distinct.len(), blocks.len());
    // Every block is writable.
    for (i, p) in blocks.iter().enumerate() {
        // SAFETY: a live block of 4096 bytes.
        unsafe { p.as_ptr().write_bytes(i as u8, 4096) };
    }
    assert!(pool.alloc().is_none());
    // SAFETY: freed once each, not used afterwards.
    unsafe { pool.free(blocks[7]) };
    assert_eq!(pool.alloc(), Some(blocks[7]));
}

#[test]
fn threads_share_a_pool() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 2000;
    let pool = Pool::new(layout(64, 8), THREADS * 16).unwrap();
    let barrier = Barrier::new(THREADS);
    std::thread::scope(|s| {
        for t in 0..THREADS {
            let (pool, barrier) = (&pool, &barrier);
            s.spawn(move || {
                barrier.wait();
                let mut held: Vec<NonNull<u8>> = Vec::new();
                for i in 0..ROUNDS {
                    if held.len() < 16 && i % 3 != 2 {
                        let p = pool.alloc().expect("room for every thread's blocks");
                        // SAFETY: a live block of 64 bytes, ours alone.
                        unsafe { p.as_ptr().write_bytes(t as u8, 64) };
                        held.push(p);
                    } else if let Some(p) = held.pop() {
                        // SAFETY: as above; nobody else wrote it.
                        unsafe {
                            let bytes = std::slice::from_raw_parts(p.as_ptr(), 64);
                            assert!(bytes.iter().all(|&b| b == t as u8), "block shared");
                            pool.free(p);
                        }
                    }
                }
                for p in held {
                    // SAFETY: as above.
                    unsafe { pool.free(p) };
                }
            });
        }
    });
    assert_eq!(pool.len(), 0);
}

#[test]
fn trims_memory_past_the_last_live_block() {
    // 64 blocks per granule.
    let block = COMMIT_GRANULE / 64;
    let mut pool = Pool::new(layout(block, 8), 64 * 8).unwrap();
    let blocks: Vec<_> = (0..64 * 8).map(|_| pool.alloc().unwrap()).collect();
    assert_eq!(pool.committed(), 8 * COMMIT_GRANULE);
    // Free everything past the first two and a half granules, and a hole below.
    // SAFETY: each block is freed once and not used afterwards.
    unsafe {
        for &p in &blocks[160..] {
            pool.free(p);
        }
        pool.free(blocks[10]);
    }
    pool.trim();
    assert_eq!(pool.committed(), 3 * COMMIT_GRANULE);
    // The hole is handed out first, then fresh blocks above the live ones.
    assert_eq!(pool.alloc(), Some(blocks[10]));
    let fresh = pool.alloc().unwrap();
    assert_eq!(fresh, blocks[160]);
    // SAFETY: a live block.
    unsafe { fresh.as_ptr().write_bytes(1, block) };
    // SAFETY: as above.
    unsafe {
        pool.free(fresh);
        for &p in &blocks[..160] {
            pool.free(p);
        }
    }
    assert!(pool.is_empty());
    pool.trim();
    assert_eq!(pool.committed(), 0);
    assert_eq!(pool.alloc(), Some(blocks[0]));
}

#[test]
fn typed_pools_drop_on_free() {
    let drops = DropCounter::new();
    struct Counted(u64, #[allow(dead_code)] DropToken);

    let pool = TypedPool::<Counted>::new(4).unwrap();
    let a = pool.alloc(Counted(1, drops.token())).ok().unwrap();
    let b = pool.alloc(Counted(2, drops.token())).ok().unwrap();
    // SAFETY: live values of this pool, each freed or taken once.
    unsafe {
        assert_eq!((a.as_ref().0, b.as_ref().0), (1, 2));
        pool.free(a);
        assert_eq!(drops.dropped(), 1);
        let taken = pool.take(b);
        assert_eq!(taken.0, 2);
        assert_eq!(drops.dropped(), 1);
        drop(taken);
    }
    assert_eq!(drops.dropped(), 2);
    assert!(pool.is_empty());

    // A full pool gives the value back.
    let full: Vec<_> = (0..pool.capacity())
        .map(|i| pool.alloc(Counted(i as u64, drops.token())).ok().unwrap())
        .collect();
    let refused = pool.alloc(Counted(99, drops.token())).err().unwrap();
    assert_eq!(refused.0, 99);
    drop(refused);
    for p in full {
        // SAFETY: as above.
        unsafe { pool.free(p) };
    }
}

#[test]
fn pools_are_allocators_for_layouts_that_fit() {
    let pool = Pool::new(layout(32, 8), 16).unwrap();
    let boxed = Box::new_in([5u64; 4], &pool);
    assert_eq!(pool.len(), 1);
    assert_eq!(*boxed, [5; 4]);
    drop(boxed);
    assert_eq!(pool.len(), 0);
    // Too big, or aligned beyond the blocks.
    assert!((&pool).allocate(layout(33, 8)).is_err());
    assert!((&pool).allocate(layout(8, 16)).is_err());
    let p = (&pool).allocate(layout(20, 4)).unwrap();
    // SAFETY: grown within the block, then freed once with the current layout.
    unsafe {
        let q = (&pool)
            .grow(p.cast(), layout(20, 4), layout(32, 8))
            .unwrap();
        assert_eq!(q.cast::<u8>(), p.cast::<u8>());
        assert!(
            (&pool)
                .grow(q.cast(), layout(32, 8), layout(40, 8))
                .is_err()
        );
        (&pool).deallocate(q.cast(), layout(32, 8));
    }
    assert_eq!(pool.len(), 0);
}
