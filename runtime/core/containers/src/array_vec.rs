//! `ArrayVec<T, N>`: a vector with fixed capacity `N` that never allocates.

use core::borrow::{Borrow, BorrowMut};
use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Deref, DerefMut, RangeBounds};
use core::{cmp, fmt, ptr, slice};

use crate::raw::{self, Drain};

/// A vector of at most `N` elements stored inline. Never allocates: the `try_` methods report a
/// full vector, the others panic.
pub struct ArrayVec<T, const N: usize> {
    len: usize,
    data: [MaybeUninit<T>; N],
}

/// Returned when an [`ArrayVec`] (or another fixed-capacity container) is full. Carries back the
/// element that did not fit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CapacityError<T = ()>(pub T);

impl<T> CapacityError<T> {
    /// The element that did not fit.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for CapacityError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapacityError")
    }
}

impl<T> fmt::Display for CapacityError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("insufficient capacity")
    }
}

impl<T> core::error::Error for CapacityError<T> {}

impl<T, const N: usize> ArrayVec<T, N> {
    pub const fn new() -> Self {
        Self {
            len: 0,
            data: [const { MaybeUninit::uninit() }; N],
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    #[inline]
    pub const fn remaining_capacity(&self) -> usize {
        N - self.len
    }

    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.data.as_ptr().cast()
    }

    #[inline]
    pub const fn as_mut_ptr(&mut self) -> *mut T {
        self.data.as_mut_ptr().cast()
    }

    #[inline]
    pub const fn as_slice(&self) -> &[T] {
        // SAFETY: the first `len` elements are initialized.
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    #[inline]
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: the first `len` elements are initialized.
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }

    /// Sets the length without touching the elements.
    ///
    /// # Safety
    /// `new_len <= N` and the first `new_len` elements are initialized.
    #[inline]
    pub unsafe fn set_len(&mut self, new_len: usize) {
        self.len = new_len;
    }

    /// Appends `value`. Panics if full.
    #[inline]
    #[track_caller]
    pub fn push(&mut self, value: T) {
        if self.try_push(value).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Appends `value`, or hands it back if full.
    #[inline]
    pub const fn try_push(&mut self, value: T) -> Result<(), CapacityError<T>> {
        if self.len == N {
            return Err(CapacityError(value));
        }
        // SAFETY: len < N.
        unsafe { self.push_unchecked(value) };
        Ok(())
    }

    /// Appends without checking the capacity.
    ///
    /// # Safety
    /// The vector is not full.
    #[inline]
    pub const unsafe fn push_unchecked(&mut self, value: T) {
        // SAFETY: the caller guarantees len < N.
        unsafe { self.as_mut_ptr().add(self.len).write(value) };
        self.len += 1;
    }

    #[inline]
    pub const fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the element at the old `len - 1` is initialized and no longer counted.
        Some(unsafe { self.as_ptr().add(self.len).read() })
    }

    /// Inserts at `index`, shifting later elements right. Panics if full or `index > len`.
    #[track_caller]
    pub fn insert(&mut self, index: usize, value: T) {
        if self.try_insert(index, value).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Inserts at `index`, or hands the value back if full. Panics if `index > len`.
    pub fn try_insert(&mut self, index: usize, value: T) -> Result<(), CapacityError<T>> {
        assert!(
            index <= self.len,
            "insert index {index} out of bounds (len {})",
            self.len
        );
        if self.len == N {
            return Err(CapacityError(value));
        }
        let base = self.as_mut_ptr();
        // SAFETY: index <= len < N.
        unsafe { raw::insert(base, &mut self.len, index, value) };
        Ok(())
    }

    /// Removes and returns the element at `index`, shifting later elements left. Panics if out of
    /// bounds.
    pub fn remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "remove index {index} out of bounds (len {})",
            self.len
        );
        let base = self.as_mut_ptr();
        // SAFETY: index < len.
        unsafe { raw::remove(base, &mut self.len, index) }
    }

    /// Removes the element at `index` and moves the last element into its place. O(1); panics if
    /// out of bounds.
    pub fn swap_remove(&mut self, index: usize) -> T {
        assert!(
            index < self.len,
            "swap_remove index {index} out of bounds (len {})",
            self.len
        );
        let base = self.as_mut_ptr();
        // SAFETY: index < len.
        unsafe { raw::swap_remove(base, &mut self.len, index) }
    }

    pub fn truncate(&mut self, len: usize) {
        let base = self.as_mut_ptr();
        // SAFETY: `base` holds `self.len` initialized elements.
        unsafe { raw::truncate(base, &mut self.len, len) };
    }

    #[inline]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Keeps only the elements for which `f` returns true, in order.
    pub fn retain(&mut self, mut f: impl FnMut(&T) -> bool) {
        self.retain_mut(|x| f(x));
    }

    /// [`retain`](Self::retain) with mutable access.
    pub fn retain_mut(&mut self, f: impl FnMut(&mut T) -> bool) {
        let base = self.as_mut_ptr();
        // SAFETY: `base` holds `self.len` initialized elements.
        unsafe { raw::retain(base, &mut self.len, f) };
    }

    /// Removes consecutive equal elements, keeping the first of each run.
    pub fn dedup(&mut self)
    where
        T: PartialEq,
    {
        self.dedup_by(|a, b| a == b);
    }

    /// Removes consecutive elements with equal keys, keeping the first of each run.
    pub fn dedup_by_key<K: PartialEq>(&mut self, mut key: impl FnMut(&mut T) -> K) {
        self.dedup_by(|a, b| key(a) == key(b));
    }

    /// Removes each element for which `same(element, previous_kept)` is true.
    pub fn dedup_by(&mut self, same: impl FnMut(&mut T, &mut T) -> bool) {
        let base = self.as_mut_ptr();
        // SAFETY: `base` holds `self.len` initialized elements.
        unsafe { raw::dedup_by(base, &mut self.len, same) };
    }

    /// Removes `range` and returns it as an iterator. Panics if the range is out of bounds.
    pub fn drain(&mut self, range: impl RangeBounds<usize>) -> Drain<'_, T> {
        let (start, end) = raw::range(range, self.len);
        let base = self.as_mut_ptr();
        // SAFETY: `base` holds `len` elements and the drain borrows the vector exclusively.
        unsafe { Drain::new(base, &mut self.len, start, end) }
    }

    /// Resizes to `new_len`, filling with clones of `value`. Panics if `new_len > N`.
    #[track_caller]
    pub fn resize(&mut self, new_len: usize, value: T)
    where
        T: Clone,
    {
        if new_len <= self.len {
            self.truncate(new_len);
            return;
        }
        if new_len > N {
            capacity_overflow::<N>();
        }
        while self.len + 1 < new_len {
            // SAFETY: len < new_len <= N.
            unsafe { self.push_unchecked(value.clone()) };
        }
        // SAFETY: as above, for the last slot.
        unsafe { self.push_unchecked(value) };
    }

    /// Resizes to `new_len`, filling with the values `f` returns. Panics if `new_len > N`.
    #[track_caller]
    pub fn resize_with(&mut self, new_len: usize, mut f: impl FnMut() -> T) {
        if new_len <= self.len {
            self.truncate(new_len);
            return;
        }
        if new_len > N {
            capacity_overflow::<N>();
        }
        while self.len < new_len {
            // SAFETY: len < new_len <= N.
            unsafe { self.push_unchecked(f()) };
        }
    }

    /// Appends clones of the elements in `range`. Panics if the range is out of bounds or the
    /// clones do not fit.
    #[track_caller]
    pub fn extend_from_within(&mut self, range: impl RangeBounds<usize>)
    where
        T: Clone,
    {
        let (start, end) = raw::range(range, self.len);
        if end - start > self.remaining_capacity() {
            capacity_overflow::<N>();
        }
        for i in start..end {
            let x = self[i].clone();
            // SAFETY: checked above that everything fits.
            unsafe { self.push_unchecked(x) };
        }
    }

    /// Appends clones of `other`. Panics if they do not all fit.
    #[track_caller]
    pub fn extend_from_slice(&mut self, other: &[T])
    where
        T: Clone,
    {
        if self.try_extend_from_slice(other).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Appends clones of `other` if they all fit; otherwise appends nothing.
    pub fn try_extend_from_slice(&mut self, other: &[T]) -> Result<(), CapacityError>
    where
        T: Clone,
    {
        if other.len() > self.remaining_capacity() {
            return Err(CapacityError(()));
        }
        for x in other {
            // SAFETY: checked above that everything fits.
            unsafe { self.push_unchecked(x.clone()) };
        }
        Ok(())
    }

    /// The elements as an array if the vector is full, else the vector back.
    pub fn into_array(self) -> Result<[T; N], Self> {
        if self.len < N {
            return Err(self);
        }
        let this = ManuallyDrop::new(self);
        // SAFETY: all N elements are initialized, and `[MaybeUninit<T>; N]` has the layout of
        // `[T; N]`; `this` is never dropped, so they are moved exactly once.
        Ok(unsafe { ptr::read(this.data.as_ptr().cast::<[T; N]>()) })
    }
}

#[cold]
#[track_caller]
fn capacity_overflow<const N: usize>() -> ! {
    panic!("ArrayVec capacity {N} exceeded");
}

impl<T, const N: usize> Drop for ArrayVec<T, N> {
    fn drop(&mut self) {
        // SAFETY: drops the `len` initialized elements.
        unsafe { ptr::drop_in_place(self.as_mut_slice()) };
    }
}

impl<T, const N: usize> Default for ArrayVec<T, N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for ArrayVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T, const N: usize> AsRef<[T]> for ArrayVec<T, N> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T, const N: usize> AsMut<[T]> for ArrayVec<T, N> {
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T, const N: usize> Borrow<[T]> for ArrayVec<T, N> {
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T, const N: usize> BorrowMut<[T]> for ArrayVec<T, N> {
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Clone, const N: usize> Clone for ArrayVec<T, N> {
    fn clone(&self) -> Self {
        let mut v = Self::new();
        v.extend_from_slice(self);
        v
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for ArrayVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl<T: PartialEq, const N: usize, const M: usize> PartialEq<ArrayVec<T, M>> for ArrayVec<T, N> {
    fn eq(&self, other: &ArrayVec<T, M>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq, const N: usize> PartialEq<[T]> for ArrayVec<T, N> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: Eq, const N: usize> Eq for ArrayVec<T, N> {}

impl<T: PartialOrd, const N: usize> PartialOrd for ArrayVec<T, N> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord, const N: usize> Ord for ArrayVec<T, N> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: Hash, const N: usize> Hash for ArrayVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl<T, const N: usize> Extend<T> for ArrayVec<T, N> {
    /// Panics if the items do not fit.
    #[track_caller]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for x in iter {
            self.push(x);
        }
    }
}

impl<T, const N: usize> FromIterator<T> for ArrayVec<T, N> {
    /// Panics if the items do not fit.
    #[track_caller]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = Self::new();
        v.extend(iter);
        v
    }
}

impl<T, const N: usize> From<[T; N]> for ArrayVec<T, N> {
    fn from(a: [T; N]) -> Self {
        let a = ManuallyDrop::new(a);
        Self {
            len: N,
            // SAFETY: `[T; N]` and `[MaybeUninit<T>; N]` have the same layout; `a` is never
            // dropped, so the elements are moved exactly once.
            data: unsafe { ptr::read((&raw const a).cast::<[MaybeUninit<T>; N]>()) },
        }
    }
}

impl<T, const N: usize> IntoIterator for ArrayVec<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;

    fn into_iter(mut self) -> IntoIter<T, N> {
        let end = self.len;
        // The iterator owns the elements now.
        self.len = 0;
        IntoIter {
            vec: self,
            start: 0,
            end,
        }
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a ArrayVec<T, N> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> slice::Iter<'a, T> {
        self.iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut ArrayVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = slice::IterMut<'a, T>;

    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.iter_mut()
    }
}

/// By-value iterator over an [`ArrayVec`].
pub struct IntoIter<T, const N: usize> {
    /// Has `len == 0`; elements `start..end` are still owned by the iterator.
    vec: ArrayVec<T, N>,
    start: usize,
    end: usize,
}

impl<T, const N: usize> IntoIter<T, N> {
    /// The elements not yet yielded.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: start..end are initialized and not yet moved out.
        unsafe { slice::from_raw_parts(self.vec.as_ptr().add(self.start), self.end - self.start) }
    }
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        if self.start == self.end {
            return None;
        }
        // SAFETY: `start < end`, so the element is initialized and is moved out exactly once.
        let value = unsafe { self.vec.as_ptr().add(self.start).read() };
        self.start += 1;
        Some(value)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.start;
        (n, Some(n))
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        if self.start == self.end {
            return None;
        }
        self.end -= 1;
        // SAFETY: as in `next`, from the other end.
        Some(unsafe { self.vec.as_ptr().add(self.end).read() })
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

impl<T, const N: usize> FusedIterator for IntoIter<T, N> {}

impl<T, const N: usize> Drop for IntoIter<T, N> {
    fn drop(&mut self) {
        // SAFETY: start <= end <= N.
        let first = unsafe { self.vec.as_mut_ptr().add(self.start) };
        let remaining = ptr::slice_from_raw_parts_mut(first, self.end - self.start);
        // Mark everything as taken first so a panicking destructor cannot cause a double drop.
        self.start = self.end;
        // SAFETY: the remaining elements are initialized and owned by the iterator.
        unsafe { ptr::drop_in_place(remaining) };
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for IntoIter<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IntoIter").field(&self.as_slice()).finish()
    }
}
