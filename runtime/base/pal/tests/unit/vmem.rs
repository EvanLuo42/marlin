#![allow(clippy::undocumented_unsafe_blocks)]

use marlin_pal::vmem;
use marlin_pal::{ErrorKind, Protection};

#[test]
fn granularities_are_powers_of_two() {
    assert!(vmem::page_size().is_power_of_two());
    assert!(vmem::allocation_granularity().is_power_of_two());
    assert!(vmem::allocation_granularity() >= vmem::page_size());
}

#[test]
fn reserve_commit_decommit_release() {
    let page = vmem::page_size();
    let size = vmem::allocation_granularity() * 4;
    let base = vmem::reserve(size).unwrap();
    assert_eq!(base.addr().get() % vmem::allocation_granularity(), 0);

    unsafe {
        // Second page only.
        let p = base.add(page);
        vmem::commit(p, page, Protection::ReadWrite).unwrap();
        assert_eq!(p.read(), 0);
        p.write(42);
        // Recommitting keeps contents.
        vmem::commit(p, page, Protection::ReadWrite).unwrap();
        assert_eq!(p.read(), 42);

        vmem::protect(p, page, Protection::Read).unwrap();
        assert_eq!(p.read(), 42);
        vmem::protect(p, page, Protection::ReadWrite).unwrap();

        // Decommitted pages come back zeroed.
        vmem::decommit(p, page).unwrap();
        vmem::commit(p, page, Protection::ReadWrite).unwrap();
        assert_eq!(p.read(), 0);

        vmem::release(base, size).unwrap();
    }
}

#[test]
fn zero_size_is_rejected() {
    assert_eq!(
        vmem::reserve(0).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[cfg(marlin_ext = "large_pages")]
#[test]
fn large_pages_when_available() {
    use marlin_pal::ext::large_pages::LargePages;

    // Usually unavailable: the account needs the "Lock pages in memory" privilege.
    let Some(lp) = LargePages::get() else {
        marlin_test_support::skip!("no large pages available to this process");
    };
    let size = lp.page_size();
    assert_eq!(
        lp.alloc(size + 1, Protection::ReadWrite)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    let p = lp.alloc(size, Protection::ReadWrite).unwrap();
    assert_eq!(p.addr().get() % size, 0);
    unsafe {
        p.write(1);
        vmem::release(p, size).unwrap();
    }
}

#[test]
fn reserve_aligned_is_aligned_and_usable() {
    let page = vmem::page_size();
    for align in [vmem::allocation_granularity(), 1 << 22, 1 << 24] {
        let size = (1 << 22) + page;
        let base = vmem::reserve_aligned(size, align).unwrap();
        assert_eq!(base.addr().get() % align, 0, "align {align:#x}");
        unsafe {
            let last = base.add(size - page);
            vmem::commit(last, page, Protection::ReadWrite).unwrap();
            last.write(1);
            vmem::release(base, size).unwrap();
        }
    }
    assert_eq!(
        vmem::reserve_aligned(1 << 20, 3 << 20).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}
