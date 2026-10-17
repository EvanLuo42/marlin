//! Element operations shared by `SmallVec` and `ArrayVec`, over a base pointer and a length, and
//! their shared [`Drain`] iterator.

use core::iter::FusedIterator;
use core::marker::PhantomData;
use core::ops::{Bound, RangeBounds};
use core::ptr::{self, NonNull};
use core::{fmt, slice};

/// Drops the elements past `new_len`.
///
/// # Safety
/// `base` points to `*len` initialized elements.
#[inline]
pub unsafe fn truncate<T>(base: *mut T, len: &mut usize, new_len: usize) {
    if new_len >= *len {
        return;
    }
    let tail = ptr::slice_from_raw_parts_mut(
        // SAFETY: new_len < len, so this stays inside the initialized range.
        unsafe { base.add(new_len) },
        *len - new_len,
    );
    // Shorten first: if a destructor panics, the rest are leaked rather than dropped twice.
    *len = new_len;
    // SAFETY: the tail was initialized and is no longer counted by `len`.
    unsafe { ptr::drop_in_place(tail) };
}

/// Inserts `value` at `index`, shifting the tail right.
///
/// # Safety
/// `base` points to `*len` initialized elements with room for one more; `index <= *len`.
#[inline]
pub unsafe fn insert<T>(base: *mut T, len: &mut usize, index: usize, value: T) {
    // SAFETY: index <= len < capacity, so `p` and `p + 1 .. len + 1` are in the allocation.
    unsafe {
        let p = base.add(index);
        ptr::copy(p, p.add(1), *len - index);
        p.write(value);
    }
    *len += 1;
}

/// Removes the element at `index`, shifting the tail left.
///
/// # Safety
/// `base` points to `*len` initialized elements; `index < *len`.
#[inline]
pub unsafe fn remove<T>(base: *mut T, len: &mut usize, index: usize) -> T {
    // SAFETY: index < len; the read moves the element out and the copy closes the gap.
    unsafe {
        let p = base.add(index);
        let value = p.read();
        ptr::copy(p.add(1), p, *len - index - 1);
        *len -= 1;
        value
    }
}

/// Removes the element at `index`, moving the last element into its place.
///
/// # Safety
/// `base` points to `*len` initialized elements; `index < *len`.
#[inline]
pub unsafe fn swap_remove<T>(base: *mut T, len: &mut usize, index: usize) -> T {
    let last = *len - 1;
    // SAFETY: index, last < len. `ptr::copy` tolerates index == last.
    unsafe {
        let value = base.add(index).read();
        ptr::copy(base.add(last), base.add(index), 1);
        *len = last;
        value
    }
}

/// Keeps the elements for which `f` returns true, preserving order.
///
/// # Safety
/// `base` points to `*len` initialized elements.
pub unsafe fn retain<T>(base: *mut T, len: &mut usize, mut f: impl FnMut(&mut T) -> bool) {
    let n = *len;
    // If `f` or a destructor panics, the remaining elements leak instead of being dropped twice.
    *len = 0;
    let mut deleted = 0;
    for i in 0..n {
        // SAFETY: i < n; elements before `i - deleted` are compacted, `i` itself is untouched.
        unsafe {
            let p = base.add(i);
            if !f(&mut *p) {
                ptr::drop_in_place(p);
                deleted += 1;
            } else if deleted > 0 {
                ptr::copy_nonoverlapping(p, base.add(i - deleted), 1);
            }
        }
    }
    *len = n - deleted;
}

/// Removes each element for which `same(element, previous_kept)` is true, keeping the first of
/// every run.
///
/// # Safety
/// `base` points to `*len` initialized elements.
pub unsafe fn dedup_by<T>(
    base: *mut T,
    len: &mut usize,
    mut same: impl FnMut(&mut T, &mut T) -> bool,
) {
    let n = *len;
    if n <= 1 {
        return;
    }
    // As in `retain`: a panic leaks the elements instead of dropping any twice.
    *len = 0;
    let mut kept = 1;
    for i in 1..n {
        // SAFETY: kept <= i < n. `0..kept` are the compacted survivors and `i` is untouched;
        // `kept - 1 < i`, so the two references never alias.
        unsafe {
            let cur = base.add(i);
            if same(&mut *cur, &mut *base.add(kept - 1)) {
                ptr::drop_in_place(cur);
            } else {
                if kept != i {
                    ptr::copy_nonoverlapping(cur, base.add(kept), 1);
                }
                kept += 1;
            }
        }
    }
    *len = kept;
}

/// `range` resolved against `len`; panics like slice indexing when it is out of bounds.
#[track_caller]
pub fn range(range: impl RangeBounds<usize>, len: usize) -> (usize, usize) {
    let start = match range.start_bound() {
        Bound::Included(&s) => s,
        Bound::Excluded(&s) => s.checked_add(1).expect("range start overflows usize"),
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&e) => e.checked_add(1).expect("range end overflows usize"),
        Bound::Excluded(&e) => e,
        Bound::Unbounded => len,
    };
    assert!(start <= end, "range starts at {start} but ends at {end}");
    assert!(end <= len, "range end {end} out of bounds (len {len})");
    (start, end)
}

/// Draining iterator of [`SmallVec`](crate::SmallVec) and [`ArrayVec`](crate::ArrayVec): yields
/// the removed range by value. Dropping it drops whatever was not yielded and closes the gap.
///
/// Leaking it (`mem::forget`) is safe: the vector's length is cut to the range start up front, so
/// the range and everything after it leak.
pub struct Drain<'a, T> {
    base: NonNull<T>,
    /// The vector's length, held at the range start while draining.
    len: &'a mut usize,
    /// Elements `next..end` are still to be yielded.
    next: usize,
    end: usize,
    /// The elements after the range, moved down on drop.
    tail_start: usize,
    tail_len: usize,
    _owns: PhantomData<T>,
}

impl<'a, T> Drain<'a, T> {
    /// # Safety
    /// `base` points to `*len` initialized elements and stays valid, with nothing else touching
    /// them, for `'a`; `start <= end <= *len`.
    #[inline]
    pub(crate) unsafe fn new(base: *mut T, len: &'a mut usize, start: usize, end: usize) -> Self {
        let old_len = *len;
        *len = start;
        Self {
            // SAFETY: the vectors' base pointers are never null.
            base: unsafe { NonNull::new_unchecked(base) },
            len,
            next: start,
            end,
            tail_start: end,
            tail_len: old_len - end,
            _owns: PhantomData,
        }
    }

    /// The elements not yet yielded.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: next..end are initialized and not yet moved out.
        unsafe { slice::from_raw_parts(self.base.as_ptr().add(self.next), self.end - self.next) }
    }
}

// SAFETY: the drain owns the yielded elements and holds the vector exclusively, like `&mut Vec`.
unsafe impl<T: Send> Send for Drain<'_, T> {}
// SAFETY: `&Drain` only hands out `&T`.
unsafe impl<T: Sync> Sync for Drain<'_, T> {}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        if self.next == self.end {
            return None;
        }
        // SAFETY: next < end, so the element is initialized and is moved out exactly once.
        let value = unsafe { self.base.as_ptr().add(self.next).read() };
        self.next += 1;
        Some(value)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.next;
        (n, Some(n))
    }
}

impl<T> DoubleEndedIterator for Drain<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        if self.next == self.end {
            return None;
        }
        self.end -= 1;
        // SAFETY: as in `next`, from the other end.
        Some(unsafe { self.base.as_ptr().add(self.end).read() })
    }
}

impl<T> ExactSizeIterator for Drain<'_, T> {}

impl<T> FusedIterator for Drain<'_, T> {}

impl<T> Drop for Drain<'_, T> {
    fn drop(&mut self) {
        /// Closes the gap even if dropping the remaining elements panics.
        struct CloseGap<'b, 'a, T>(&'b mut Drain<'a, T>);

        impl<T> Drop for CloseGap<'_, '_, T> {
            fn drop(&mut self) {
                let d = &mut *self.0;
                let start = *d.len;
                if d.tail_len > 0 && d.tail_start != start {
                    // SAFETY: both ranges lie within the original elements; `copy` allows the
                    // overlap.
                    unsafe {
                        let base = d.base.as_ptr();
                        ptr::copy(base.add(d.tail_start), base.add(start), d.tail_len);
                    }
                }
                *d.len = start + d.tail_len;
            }
        }

        let remaining = ptr::slice_from_raw_parts_mut(
            // SAFETY: next <= end <= the original length.
            unsafe { self.base.as_ptr().add(self.next) },
            self.end - self.next,
        );
        self.next = self.end;
        let guard = CloseGap(self);
        // SAFETY: the remaining elements are initialized and owned by the drain; `next == end`
        // means nothing else drops them.
        unsafe { ptr::drop_in_place(remaining) };
        drop(guard);
    }
}

impl<T: fmt::Debug> fmt::Debug for Drain<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Drain").field(&self.as_slice()).finish()
    }
}
