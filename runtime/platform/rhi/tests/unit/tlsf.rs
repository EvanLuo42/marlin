//! The TLSF suballocator: placement, alignment, merging, exhaustion, and a long random run
//! checked against the ranges it handed out.

use marlin_rhi::{Tlsf, TlsfAllocation};

#[test]
fn allocations_are_disjoint_and_freeing_merges_them_back() {
    let mut t = Tlsf::new(1024);
    let a = t.allocate(100, 1).unwrap();
    let b = t.allocate(200, 1).unwrap();
    let c = t.allocate(300, 1).unwrap();
    assert!(a.offset + a.size <= b.offset || b.offset + b.size <= a.offset);
    assert_eq!(t.used(), 600);
    assert_eq!(t.allocation_count(), 3);
    // Free the middle one first: it cannot merge until its neighbors are free.
    t.free(b);
    t.free(a);
    t.free(c);
    assert!(t.is_empty());
    assert_eq!(t.used(), 0);
    assert_eq!(t.largest_free(), 1024);
    // The whole range is one free range again.
    let all = t.allocate(1024, 1).unwrap();
    assert_eq!((all.offset, all.size), (0, 1024));
}

#[test]
fn offsets_honor_alignment() {
    let mut t = Tlsf::new(1 << 20);
    let _odd = t.allocate(3, 1).unwrap();
    for align in [4, 16, 256, 4096, 65536] {
        let a = t.allocate(100, align).unwrap();
        assert_eq!(a.offset % align, 0, "align {align}");
        assert_eq!(a.size, 100);
    }
}

#[test]
fn padding_before_an_aligned_allocation_stays_usable() {
    let mut t = Tlsf::new(8192);
    let _first = t.allocate(1, 1).unwrap();
    let aligned = t.allocate(4096, 4096).unwrap();
    assert_eq!(aligned.offset, 4096);
    // Bytes 1..4096 were padding: still free.
    let gap = t.allocate(4095, 1).unwrap();
    assert_eq!(gap.offset, 1);
}

#[test]
fn exhaustion_returns_none_and_recovers() {
    let mut t = Tlsf::new(4096);
    let all: Vec<TlsfAllocation> = (0..16).map(|_| t.allocate(256, 256).unwrap()).collect();
    assert_eq!(t.largest_free(), 0);
    assert!(t.allocate(1, 1).is_none());
    assert!(t.allocate(u64::MAX, 1).is_none());
    for a in all {
        t.free(a);
    }
    assert!(t.allocate(4096, 4096).is_some());
}

#[test]
fn request_larger_than_the_range_fails() {
    let mut t = Tlsf::new(1000);
    assert!(t.allocate(1001, 1).is_none());
    // Fits only without alignment padding, which the start of the range does not need.
    assert!(t.allocate(1000, 512).is_some());
    let mut empty = Tlsf::new(0);
    assert!(empty.allocate(1, 1).is_none());
}

#[test]
fn zero_sized_requests_take_a_byte() {
    let mut t = Tlsf::new(16);
    let a = t.allocate(0, 1).unwrap();
    assert_eq!(a.size, 1);
}

#[test]
#[should_panic(expected = "freed once")]
fn freeing_twice_panics() {
    let mut t = Tlsf::new(64);
    let a = t.allocate(8, 1).unwrap();
    let copy = TlsfAllocationCopy::of(&a);
    t.free(a);
    t.free(copy.0);
}

/// A second handle to the same allocation, to free it twice (only possible through `unsafe`).
struct TlsfAllocationCopy(TlsfAllocation);

impl TlsfAllocationCopy {
    fn of(a: &TlsfAllocation) -> Self {
        // SAFETY: `TlsfAllocation` is plain data; the copy is used only to test the double-free
        // check.
        Self(unsafe { core::ptr::read(a) })
    }
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn random_allocations_never_overlap() {
    const SIZE: u64 = 1 << 30;
    let mut t = Tlsf::new(SIZE);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut live: Vec<TlsfAllocation> = Vec::new();
    let mut failures = 0;
    for step in 0..20_000 {
        if !live.is_empty() && (rng.below(100) < 45 || live.len() > 2000) {
            let i = rng.below(live.len() as u64) as usize;
            t.free(live.swap_remove(i));
        } else {
            // Mostly small, sometimes large; GPU-like alignments.
            let size = match rng.below(10) {
                0 => 1 + rng.below(4 << 20),
                1..=3 => 1 + rng.below(64 << 10),
                _ => 1 + rng.below(4096),
            };
            let align = 1 << rng.below(17);
            match t.allocate(size, align) {
                Some(a) => {
                    assert_eq!(a.offset % align, 0);
                    assert!(a.offset + a.size <= SIZE);
                    live.push(a);
                }
                None => failures += 1,
            }
        }
        if step % 1000 == 0 {
            let mut ranges: Vec<(u64, u64)> = live.iter().map(|a| (a.offset, a.size)).collect();
            ranges.sort_unstable();
            for w in ranges.windows(2) {
                assert!(w[0].0 + w[0].1 <= w[1].0, "overlap: {w:?}");
            }
            assert_eq!(t.used(), ranges.iter().map(|r| r.1).sum::<u64>());
            assert_eq!(t.allocation_count() as usize, live.len());
        }
    }
    // The range never filled up enough to refuse a request.
    assert_eq!(failures, 0);
    for a in live.drain(..) {
        t.free(a);
    }
    assert!(t.is_empty());
    assert_eq!(t.largest_free(), SIZE);
}
