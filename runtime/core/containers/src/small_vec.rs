//! `SmallVec<T, N>`: a vector that keeps up to `N` elements inline.

use alloc::alloc::{Layout, alloc, dealloc, handle_alloc_error, realloc};
use alloc::vec::Vec;
use core::borrow::{Borrow, BorrowMut};
use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::mem::{ManuallyDrop, MaybeUninit, size_of};
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::{cmp, fmt, slice};

use crate::raw;

/// A vector that stores up to `N` elements inline and moves them to the heap when it outgrows
/// that ("spills"). Once spilled it stays on the heap until [`shrink_to_fit`](Self::shrink_to_fit).
///
/// Zero-sized types never allocate.
pub struct SmallVec<T, const N: usize> {
    /// Heap capacity while spilled, `N` while inline: spilled iff `cap > N`.
    cap: usize,
    len: usize,
    data: Data<T, N>,
}

union Data<T, const N: usize> {
    inline: ManuallyDrop<[MaybeUninit<T>; N]>,
    heap: NonNull<T>,
}

// SAFETY: SmallVec owns its elements like Vec does; the raw pointer is never shared.
unsafe impl<T: Send, const N: usize> Send for SmallVec<T, N> {}
// SAFETY: as above; `&SmallVec` only hands out `&T`.
unsafe impl<T: Sync, const N: usize> Sync for SmallVec<T, N> {}

impl<T, const N: usize> SmallVec<T, N> {
    /// An empty vector with inline capacity `N`.
    #[inline]
    pub const fn new() -> Self {
        Self {
            cap: N,
            len: 0,
            data: Data {
                inline: ManuallyDrop::new([const { MaybeUninit::uninit() }; N]),
            },
        }
    }

    /// An empty vector with room for at least `capacity` elements.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut v = Self::new();
        v.reserve(capacity);
        v
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Elements that fit without reallocating.
    #[inline]
    pub const fn capacity(&self) -> usize {
        if size_of::<T>() == 0 {
            usize::MAX
        } else {
            self.cap
        }
    }

    /// The inline capacity, `N`.
    #[inline]
    pub const fn inline_capacity(&self) -> usize {
        N
    }

    /// Whether the elements live on the heap.
    #[inline]
    pub const fn spilled(&self) -> bool {
        self.cap > N
    }

    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        if self.spilled() {
            // SAFETY: `heap` is the active field while spilled.
            unsafe { self.data.heap.as_ptr() }
        } else {
            (&raw const self.data.inline).cast()
        }
    }

    #[inline]
    pub const fn as_mut_ptr(&mut self) -> *mut T {
        if self.spilled() {
            // SAFETY: `heap` is the active field while spilled.
            unsafe { self.data.heap.as_ptr() }
        } else {
            (&raw mut self.data.inline).cast()
        }
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
    /// `new_len <= capacity()` and the first `new_len` elements are initialized.
    #[inline]
    pub unsafe fn set_len(&mut self, new_len: usize) {
        self.len = new_len;
    }

    #[inline]
    pub fn push(&mut self, value: T) {
        if self.len == self.capacity() {
            self.grow(1);
        }
        // SAFETY: len < capacity.
        unsafe { self.as_mut_ptr().add(self.len).write(value) };
        self.len += 1;
    }

    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the element at the old `len - 1` is initialized and no longer counted.
        Some(unsafe { self.as_ptr().add(self.len).read() })
    }

    /// Inserts at `index`, shifting later elements right. Panics if `index > len`.
    pub fn insert(&mut self, index: usize, value: T) {
        assert!(
            index <= self.len,
            "insert index {index} out of bounds (len {})",
            self.len
        );
        if self.len == self.capacity() {
            self.grow(1);
        }
        let base = self.as_mut_ptr();
        // SAFETY: index <= len < capacity.
        unsafe { raw::insert(base, &mut self.len, index, value) };
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

    /// Drops the elements past `len`. Keeps the capacity.
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

    /// Makes room for at least `additional` more elements.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        if self.capacity() - self.len < additional {
            self.grow(additional);
        }
    }

    /// Moves back inline if the elements fit, else shrinks the heap block to `len`.
    pub fn shrink_to_fit(&mut self) {
        if !self.spilled() {
            return;
        }
        // SAFETY: spilled, so `heap` is active and was allocated with `layout(cap)`.
        unsafe {
            let heap = self.data.heap;
            let old = Self::layout(self.cap);
            if self.len <= N {
                self.data = Data {
                    inline: ManuallyDrop::new([const { MaybeUninit::uninit() }; N]),
                };
                let inline = (&raw mut self.data.inline).cast::<T>();
                ptr::copy_nonoverlapping(heap.as_ptr(), inline, self.len);
                self.cap = N;
                dealloc(heap.as_ptr().cast(), old);
            } else if self.len < self.cap {
                let new = Self::layout(self.len);
                let p = realloc(heap.as_ptr().cast(), old, new.size());
                if p.is_null() {
                    handle_alloc_error(new);
                }
                self.data.heap = NonNull::new_unchecked(p.cast());
                self.cap = self.len;
            }
        }
    }

    /// Converts into a `Vec`, reusing the heap block when spilled.
    pub fn into_vec(self) -> Vec<T> {
        let mut this = ManuallyDrop::new(self);
        if this.spilled() {
            // SAFETY: the block came from the global allocator with `Layout::array::<T>(cap)`,
            // which is exactly what Vec expects; `this` is never dropped.
            unsafe { Vec::from_raw_parts(this.data.heap.as_ptr(), this.len, this.cap) }
        } else {
            let mut v = Vec::with_capacity(this.len);
            // SAFETY: moves the `len` inline elements; `this` is never dropped.
            unsafe {
                ptr::copy_nonoverlapping(this.as_mut_ptr(), v.as_mut_ptr(), this.len);
                v.set_len(this.len);
            }
            v
        }
    }

    /// Appends clones of every element of `other`.
    pub fn extend_from_slice(&mut self, other: &[T])
    where
        T: Clone,
    {
        self.reserve(other.len());
        for x in other {
            self.push(x.clone());
        }
    }

    #[inline]
    fn layout(cap: usize) -> Layout {
        Layout::array::<T>(cap).expect("SmallVec capacity overflow")
    }

    #[cold]
    #[inline(never)]
    fn grow(&mut self, additional: usize) {
        let required = self
            .len
            .checked_add(additional)
            .expect("SmallVec capacity overflow");
        if size_of::<T>() == 0 {
            return;
        }
        // required > cap >= N, so the result is always spilled.
        let new_cap = cmp::max(cmp::max(required, self.cap.saturating_mul(2)), 4);
        let new = Self::layout(new_cap);
        // SAFETY: `new` has non-zero size (T is not a ZST, new_cap >= 4). When spilled, the old
        // block was allocated with `layout(cap)`; when inline, `len` elements are moved out.
        unsafe {
            let p = if self.spilled() {
                realloc(
                    self.data.heap.as_ptr().cast(),
                    Self::layout(self.cap),
                    new.size(),
                )
            } else {
                let p = alloc(new);
                if !p.is_null() {
                    ptr::copy_nonoverlapping(self.as_ptr(), p.cast::<T>(), self.len);
                }
                p
            };
            if p.is_null() {
                handle_alloc_error(new);
            }
            self.data.heap = NonNull::new_unchecked(p.cast());
        }
        self.cap = new_cap;
    }
}

impl<T, const N: usize> Drop for SmallVec<T, N> {
    fn drop(&mut self) {
        // SAFETY: drops the `len` initialized elements, then frees the block if spilled.
        unsafe {
            ptr::drop_in_place(self.as_mut_slice());
            if self.spilled() {
                dealloc(self.data.heap.as_ptr().cast(), Self::layout(self.cap));
            }
        }
    }
}

impl<T, const N: usize> Default for SmallVec<T, N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for SmallVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> DerefMut for SmallVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T, const N: usize> AsRef<[T]> for SmallVec<T, N> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T, const N: usize> AsMut<[T]> for SmallVec<T, N> {
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T, const N: usize> Borrow<[T]> for SmallVec<T, N> {
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T, const N: usize> BorrowMut<[T]> for SmallVec<T, N> {
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Clone, const N: usize> Clone for SmallVec<T, N> {
    fn clone(&self) -> Self {
        let mut v = Self::new();
        v.extend_from_slice(self);
        v
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for SmallVec<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl<T: PartialEq, const N: usize, const M: usize> PartialEq<SmallVec<T, M>> for SmallVec<T, N> {
    fn eq(&self, other: &SmallVec<T, M>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq, const N: usize> PartialEq<[T]> for SmallVec<T, N> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: Eq, const N: usize> Eq for SmallVec<T, N> {}

impl<T: PartialOrd, const N: usize> PartialOrd for SmallVec<T, N> {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord, const N: usize> Ord for SmallVec<T, N> {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: Hash, const N: usize> Hash for SmallVec<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl<T, const N: usize> Extend<T> for SmallVec<T, N> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for x in iter {
            self.push(x);
        }
    }
}

impl<'a, T: Copy + 'a, const N: usize> Extend<&'a T> for SmallVec<T, N> {
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        self.extend(iter.into_iter().copied());
    }
}

impl<T, const N: usize> FromIterator<T> for SmallVec<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = Self::new();
        v.extend(iter);
        v
    }
}

impl<T: Clone, const N: usize> From<&[T]> for SmallVec<T, N> {
    fn from(s: &[T]) -> Self {
        let mut v = Self::new();
        v.extend_from_slice(s);
        v
    }
}

impl<T, const N: usize, const M: usize> From<[T; M]> for SmallVec<T, N> {
    fn from(a: [T; M]) -> Self {
        a.into_iter().collect()
    }
}

impl<T, const N: usize> From<Vec<T>> for SmallVec<T, N> {
    /// Takes over the `Vec`'s heap block when it is larger than the inline capacity.
    fn from(v: Vec<T>) -> Self {
        if size_of::<T>() == 0 || v.capacity() <= N {
            return v.into_iter().collect();
        }
        let mut v = ManuallyDrop::new(v);
        // SAFETY: a Vec's pointer is non-null, and with capacity > 0 and a non-ZST it was
        // allocated as `Layout::array::<T>(capacity)`, which our Drop frees identically.
        let heap = unsafe { NonNull::new_unchecked(v.as_mut_ptr()) };
        Self {
            cap: v.capacity(),
            len: v.len(),
            data: Data { heap },
        }
    }
}

impl<T, const N: usize> IntoIterator for SmallVec<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;

    fn into_iter(mut self) -> IntoIter<T, N> {
        let end = self.len;
        // The iterator owns the elements now; the vector only frees memory when dropped.
        self.len = 0;
        IntoIter {
            vec: self,
            start: 0,
            end,
        }
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a SmallVec<T, N> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> slice::Iter<'a, T> {
        self.iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut SmallVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = slice::IterMut<'a, T>;

    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.iter_mut()
    }
}

/// By-value iterator over a [`SmallVec`].
pub struct IntoIter<T, const N: usize> {
    /// Has `len == 0`; elements `start..end` are still owned by the iterator.
    vec: SmallVec<T, N>,
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
        // SAFETY: start <= end <= capacity.
        let first = unsafe { self.vec.as_mut_ptr().add(self.start) };
        let remaining = ptr::slice_from_raw_parts_mut(first, self.end - self.start);
        // Mark everything as taken first so a panicking destructor cannot cause a double drop.
        self.start = self.end;
        // SAFETY: the remaining elements are initialized and owned by the iterator; `vec` (with
        // len 0) then frees only the memory.
        unsafe { ptr::drop_in_place(remaining) };
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for IntoIter<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IntoIter").field(&self.as_slice()).finish()
    }
}
