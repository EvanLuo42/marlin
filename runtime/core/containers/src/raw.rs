//! Element operations shared by `SmallVec` and `ArrayVec`, over a base pointer and a length.

use core::ptr;

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
