//! Segment geometry and size classes.

pub(crate) const SEGMENT_SHIFT: u32 = 22;
/// 4 MiB. Segments are aligned to their size, so masking a pointer finds its segment header.
pub(crate) const SEGMENT_SIZE: usize = 1 << SEGMENT_SHIFT;
/// 64 KiB pages for small blocks.
pub(crate) const SMALL_PAGE_SHIFT: u32 = 16;
/// 512 KiB pages for medium blocks.
pub(crate) const MEDIUM_PAGE_SHIFT: u32 = 19;
pub(crate) const MAX_PAGES: usize = SEGMENT_SIZE >> SMALL_PAGE_SHIFT;

/// Largest block in a small page: at least 7 blocks per page, even page 0 after the header.
pub(crate) const SMALL_MAX: usize = 8 << 10;
/// Largest block in a medium page. Larger requests get a huge segment of their own.
pub(crate) const MEDIUM_MAX: usize = 64 << 10;

/// Bins 1..=48 cover 8 B..=64 KiB; bin 0 is unused.
pub(crate) const BIN_COUNT: usize = 49;

/// Size class of a request of `size` bytes, `1..=MEDIUM_MAX`.
///
/// Up to 64 bytes bins step by 8. Above, each power-of-two range is split into 4 bins, which caps
/// internal waste at 25%. Every bin above 64 bytes is a multiple of 16.
#[inline]
pub(crate) const fn bin(size: usize) -> usize {
    if size <= 64 {
        size.div_ceil(8)
    } else {
        let w = size - 1;
        // floor(log2(w)) >= 6
        let b = (usize::BITS - 1 - w.leading_zeros()) as usize;
        let k = (w >> (b - 2)) & 3;
        9 + (b - 6) * 4 + k
    }
}

/// Block size of `bin`.
#[inline]
pub(crate) const fn bin_size(bin: usize) -> usize {
    if bin <= 8 {
        bin * 8
    } else {
        let b = 6 + (bin - 9) / 4;
        let k = (bin - 9) % 4;
        (1 << b) + ((k + 1) << (b - 2))
    }
}

const _: () = assert!(bin(MEDIUM_MAX) == BIN_COUNT - 1);
const _: () = assert!(bin_size(BIN_COUNT - 1) == MEDIUM_MAX);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_cover_every_size_tightly() {
        for size in 1..=MEDIUM_MAX {
            let b = bin(size);
            assert!((1..BIN_COUNT).contains(&b), "size {size}");
            assert!(bin_size(b) >= size, "size {size}");
            assert!(b == 1 || bin_size(b - 1) < size, "size {size}");
            if size > 64 {
                assert!(
                    bin_size(b) * 4 <= size * 5 + 64,
                    "size {size} wastes too much"
                );
                assert_eq!(bin_size(b) % 16, 0);
            }
        }
    }

    #[test]
    fn bin_sizes_increase() {
        for b in 2..BIN_COUNT {
            assert!(bin_size(b) > bin_size(b - 1));
        }
    }
}
