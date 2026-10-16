//! Helpers shared by the codecs.

/// LZ77 match copy inside `buf`: `len` bytes from `pos - offset` to `pos`, where the ranges may
/// overlap (`offset < len` repeats the last `offset` bytes). The caller has checked
/// `1 <= offset <= pos` and `pos + len <= buf.len()`.
#[inline]
pub(crate) fn copy_match(buf: &mut [u8], pos: usize, offset: usize, len: usize) {
    debug_assert!(offset >= 1 && offset <= pos && pos + len <= buf.len());
    if offset >= len {
        buf.copy_within(pos - offset..pos - offset + len, pos);
        return;
    }
    if offset == 1 {
        let b = buf[pos - 1];
        buf[pos..pos + len].fill(b);
        return;
    }
    // `buf[pos - offset..pos + done]` has period `offset`, so any multiple of `offset` up to
    // `done + offset` back is a valid source; the largest one makes the chunks double.
    let mut done = 0;
    while done < len {
        let back = (done + offset) / offset * offset;
        let n = back.min(len - done);
        let at = pos + done;
        buf.copy_within(at - back..at - back + n, at);
        done += n;
    }
}

#[inline]
pub(crate) fn le16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*b.get(i..)?.first_chunk::<2>()?))
}

#[inline]
pub(crate) fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*b.get(i..)?.first_chunk::<4>()?))
}

#[inline]
pub(crate) fn le64(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(*b.get(i..)?.first_chunk::<8>()?))
}
