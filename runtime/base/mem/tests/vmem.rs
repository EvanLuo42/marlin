//! `Reservation`: page-granular commits with exact tracking.

use marlin_mem::vmem::{Protection, Reservation, allocation_granularity, page_size};

#[test]
fn tracks_committed_pages_exactly() {
    let page = page_size();
    let r = Reservation::new(64 * page).unwrap();
    assert!(r.size() >= 64 * page);
    assert_eq!(r.size() % allocation_granularity(), 0);
    assert_eq!(r.committed(), 0);
    assert!(!r.is_committed(0));

    r.commit(0, page, Protection::ReadWrite).unwrap();
    assert_eq!(r.committed(), page);
    // Overlapping commits count each page once; lengths round up to whole pages.
    r.commit(0, 3 * page - 1, Protection::ReadWrite).unwrap();
    assert_eq!(r.committed(), 3 * page);
    r.commit(2 * page, 2 * page, Protection::ReadWrite).unwrap();
    assert_eq!(r.committed(), 4 * page);
    assert!(r.is_committed(3 * page + 5));
    assert!(!r.is_committed(4 * page));

    // Committed memory is usable and starts zeroed.
    // SAFETY: the first four pages are committed read-write.
    let bytes = unsafe { std::slice::from_raw_parts_mut(r.as_ptr(), 4 * page) };
    assert!(bytes.iter().all(|&b| b == 0));
    bytes.fill(0x5A);

    // SAFETY: nothing touches page 1 until it is committed again.
    unsafe { r.decommit(page, page).unwrap() };
    assert_eq!(r.committed(), 3 * page);
    assert!(!r.is_committed(page));
    // Decommitting what is not committed changes nothing.
    // SAFETY: as above, and pages 10..12 were never used.
    unsafe {
        r.decommit(page, page).unwrap();
        r.decommit(10 * page, 2 * page).unwrap();
    }
    assert_eq!(r.committed(), 3 * page);

    r.commit(page, page, Protection::ReadWrite).unwrap();
    // SAFETY: committed again: fresh zeroes, its neighbors kept.
    unsafe {
        assert_eq!(r.as_ptr().add(page).read(), 0);
        assert_eq!(r.as_ptr().read(), 0x5A);
        assert_eq!(r.as_ptr().add(2 * page).read(), 0x5A);
    }
}

#[test]
fn rejects_bad_ranges() {
    let page = page_size();
    let r = Reservation::new(16 * page).unwrap();
    let size = r.size();
    assert!(r.commit(1, page, Protection::ReadWrite).is_err());
    assert!(r.commit(0, 0, Protection::ReadWrite).is_err());
    assert!(r.commit(0, size + 1, Protection::ReadWrite).is_err());
    assert!(r.commit(size, page, Protection::ReadWrite).is_err());
    assert!(r.commit(page, usize::MAX, Protection::ReadWrite).is_err());
    assert_eq!(r.committed(), 0);
    // Protecting needs every page committed.
    r.commit(0, page, Protection::ReadWrite).unwrap();
    // SAFETY: nothing references the reservation's memory.
    unsafe {
        assert!(r.protect(0, 2 * page, Protection::Read).is_err());
        r.protect(0, page, Protection::Read).unwrap();
        r.protect(0, page, Protection::ReadWrite).unwrap();
    }
    // SAFETY: page 0 is read-write again.
    unsafe { r.as_ptr().write(1) };
    assert!(r.contains(r.as_ptr().wrapping_add(size - 1)));
    assert!(!r.contains(r.as_ptr().wrapping_add(size)));
}

#[test]
fn large_sparse_reservations() {
    let page = page_size();
    // 16 GiB of address space; only the bitmap pages touched get committed.
    let r = Reservation::with_alignment(16 << 30, 1 << 21).unwrap();
    assert_eq!(r.as_ptr().addr() % (1 << 21), 0);
    for gib in [0usize, 5, 11, 15] {
        let offset = gib << 30;
        r.commit(offset, 2 * page, Protection::ReadWrite).unwrap();
        // SAFETY: just committed.
        unsafe { r.as_ptr().add(offset + page).write(gib as u8) };
    }
    assert_eq!(r.committed(), 8 * page);
    assert!(r.is_committed(11 << 30));
    assert!(!r.is_committed(12 << 30));
}

#[test]
fn threads_commit_concurrently() {
    let page = page_size();
    let r = Reservation::new(256 * page).unwrap();
    std::thread::scope(|s| {
        for t in 0..8 {
            let r = &r;
            s.spawn(move || {
                for round in 0..50 {
                    // Overlapping ranges: page t..t+8 every round.
                    let first = (t * 4 + round) % 200;
                    r.commit(first * page, 8 * page, Protection::ReadWrite)
                        .unwrap();
                }
            });
        }
    });
    let committed = (0..r.size() / page)
        .filter(|&p| r.is_committed(p * page))
        .count();
    assert_eq!(r.committed(), committed * page);
}
