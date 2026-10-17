//! `ArrayDeque<T, N>`: a ring buffer with fixed capacity `N` that never allocates.

use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};
use core::{fmt, ptr, slice};

use crate::CapacityError;

/// A double-ended queue of at most `N` elements stored inline, as a ring. Never allocates: the
/// `try_` methods report a full queue, `push_*` panic, and
/// [`push_back_overwrite`](Self::push_back_overwrite) evicts the oldest element (a history
/// buffer).
///
/// For a growable ring use [`Deque`](crate::Deque).
pub struct ArrayDeque<T, const N: usize> {
    /// Physical index of the front element.
    head: usize,
    len: usize,
    data: [MaybeUninit<T>; N],
}

impl<T, const N: usize> ArrayDeque<T, N> {
    pub const fn new() -> Self {
        Self {
            head: 0,
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

    /// The physical slot of logical index `i` (`i < 2N`, so one subtraction wraps it).
    #[inline(always)]
    const fn slot(&self, i: usize) -> usize {
        let p = self.head + i;
        if p >= N { p - N } else { p }
    }

    #[inline(always)]
    fn ptr_at(&self, slot: usize) -> *const T {
        // SAFETY: callers pass slot <= N; slot N (or 0 when N == 0) only for empty runs.
        unsafe { self.data.as_ptr().add(slot).cast() }
    }

    #[inline(always)]
    fn mut_ptr_at(&mut self, slot: usize) -> *mut T {
        // SAFETY: callers pass slot <= N; slot N (or 0 when N == 0) only for empty runs.
        unsafe { self.data.as_mut_ptr().add(slot).cast() }
    }

    /// Appends at the back. Panics if full.
    #[inline]
    #[track_caller]
    pub fn push_back(&mut self, value: T) {
        if self.try_push_back(value).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Prepends at the front. Panics if full.
    #[inline]
    #[track_caller]
    pub fn push_front(&mut self, value: T) {
        if self.try_push_front(value).is_err() {
            capacity_overflow::<N>();
        }
    }

    /// Appends at the back, or hands the value back if full.
    #[inline]
    pub fn try_push_back(&mut self, value: T) -> Result<(), CapacityError<T>> {
        if self.len == N {
            return Err(CapacityError(value));
        }
        let slot = self.slot(self.len);
        // SAFETY: len < N, so the slot is free.
        unsafe { self.mut_ptr_at(slot).write(value) };
        self.len += 1;
        Ok(())
    }

    /// Prepends at the front, or hands the value back if full.
    #[inline]
    pub fn try_push_front(&mut self, value: T) -> Result<(), CapacityError<T>> {
        if self.len == N {
            return Err(CapacityError(value));
        }
        // N > 0 here, since len < N.
        self.head = if self.head == 0 { N - 1 } else { self.head - 1 };
        let head = self.head;
        // SAFETY: len < N, so the slot before the old head is free.
        unsafe { self.mut_ptr_at(head).write(value) };
        self.len += 1;
        Ok(())
    }

    /// Appends at the back; when full, first removes and returns the front element. With
    /// `N == 0` the value itself comes back.
    pub fn push_back_overwrite(&mut self, value: T) -> Option<T> {
        if N == 0 {
            return Some(value);
        }
        let evicted = if self.len == N {
            self.pop_front()
        } else {
            None
        };
        // Not full now.
        let _ = self.try_push_back(value);
        evicted
    }

    #[inline]
    pub fn pop_front(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let head = self.head;
        self.head = self.slot(1);
        self.len -= 1;
        // SAFETY: the old front was initialized and is no longer counted.
        Some(unsafe { self.ptr_at(head).read() })
    }

    #[inline]
    pub fn pop_back(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        let slot = self.slot(self.len);
        // SAFETY: the old back was initialized and is no longer counted.
        Some(unsafe { self.ptr_at(slot).read() })
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        // SAFETY: index < len, so the slot is initialized.
        Some(unsafe { &*self.ptr_at(self.slot(index)) })
    }

    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        let slot = self.slot(index);
        // SAFETY: index < len, so the slot is initialized.
        Some(unsafe { &mut *self.mut_ptr_at(slot) })
    }

    #[inline]
    pub fn front(&self) -> Option<&T> {
        self.get(0)
    }

    #[inline]
    pub fn back(&self) -> Option<&T> {
        self.len.checked_sub(1).and_then(|i| self.get(i))
    }

    #[inline]
    pub fn front_mut(&mut self) -> Option<&mut T> {
        self.get_mut(0)
    }

    #[inline]
    pub fn back_mut(&mut self) -> Option<&mut T> {
        self.len.checked_sub(1).and_then(|i| self.get_mut(i))
    }

    /// The elements in order, as the run up to the end of the array and the wrapped-around rest.
    pub fn as_slices(&self) -> (&[T], &[T]) {
        let first = self.len.min(N - self.head);
        // SAFETY: both runs are initialized: `head..head + first` and `0..len - first`.
        unsafe {
            (
                slice::from_raw_parts(self.ptr_at(self.head), first),
                slice::from_raw_parts(self.ptr_at(0), self.len - first),
            )
        }
    }

    /// [`as_slices`](Self::as_slices), mutably.
    pub fn as_mut_slices(&mut self) -> (&mut [T], &mut [T]) {
        let first = self.len.min(N - self.head);
        let rest = self.len - first;
        let base = self.data.as_mut_ptr().cast::<T>();
        // SAFETY: the runs are initialized and disjoint (`first + rest <= N`).
        unsafe {
            (
                slice::from_raw_parts_mut(base.add(self.head), first),
                slice::from_raw_parts_mut(base, rest),
            )
        }
    }

    /// Moves the elements so they are contiguous from slot 0 and returns them as one slice.
    pub fn make_contiguous(&mut self) -> &mut [T] {
        if self.head + self.len > N {
            // [back run][free][front run]: rotating the raw slots puts the front run first.
            self.data.rotate_left(self.head);
        } else if self.head != 0 && self.len > 0 {
            // SAFETY: the run is initialized and inside the array; `copy` allows the overlap.
            unsafe {
                let base = self.data.as_mut_ptr().cast::<T>();
                ptr::copy(base.add(self.head), base, self.len);
            }
        }
        self.head = 0;
        // SAFETY: the `len` elements now start at slot 0.
        unsafe { slice::from_raw_parts_mut(self.data.as_mut_ptr().cast(), self.len) }
    }

    pub fn clear(&mut self) {
        let (a, b) = self.as_mut_slices();
        let (a, b) = (ptr::from_mut(a), ptr::from_mut(b));
        // Forget the elements first so a panicking destructor cannot cause a double drop.
        self.len = 0;
        self.head = 0;
        // SAFETY: both runs were initialized and are no longer counted.
        unsafe {
            ptr::drop_in_place(a);
            ptr::drop_in_place(b);
        }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        let (a, b) = self.as_slices();
        Iter::new(a, b)
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        let (a, b) = self.as_mut_slices();
        IterMut::new(a, b)
    }
}

#[cold]
#[track_caller]
fn capacity_overflow<const N: usize>() -> ! {
    panic!("ArrayDeque capacity {N} exceeded");
}

impl<T, const N: usize> Drop for ArrayDeque<T, N> {
    fn drop(&mut self) {
        self.clear();
    }
}

impl<T, const N: usize> Default for ArrayDeque<T, N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone, const N: usize> Clone for ArrayDeque<T, N> {
    fn clone(&self) -> Self {
        self.iter().cloned().collect()
    }
}

impl<T: fmt::Debug, const N: usize> fmt::Debug for ArrayDeque<T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: PartialEq, const N: usize, const M: usize> PartialEq<ArrayDeque<T, M>>
    for ArrayDeque<T, N>
{
    fn eq(&self, other: &ArrayDeque<T, M>) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq, const N: usize> Eq for ArrayDeque<T, N> {}

impl<T: Hash, const N: usize> Hash for ArrayDeque<T, N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len);
        for x in self {
            x.hash(state);
        }
    }
}

impl<T, const N: usize> Index<usize> for ArrayDeque<T, N> {
    type Output = T;

    #[track_caller]
    fn index(&self, index: usize) -> &T {
        match self.get(index) {
            Some(x) => x,
            None => panic!("index {index} out of bounds (len {})", self.len),
        }
    }
}

impl<T, const N: usize> IndexMut<usize> for ArrayDeque<T, N> {
    #[track_caller]
    fn index_mut(&mut self, index: usize) -> &mut T {
        let len = self.len;
        match self.get_mut(index) {
            Some(x) => x,
            None => panic!("index {index} out of bounds (len {len})"),
        }
    }
}

impl<T, const N: usize> Extend<T> for ArrayDeque<T, N> {
    /// Pushes at the back; panics if the items do not fit.
    #[track_caller]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for x in iter {
            self.push_back(x);
        }
    }
}

impl<T, const N: usize> FromIterator<T> for ArrayDeque<T, N> {
    /// Panics if the items do not fit.
    #[track_caller]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut d = Self::new();
        d.extend(iter);
        d
    }
}

/// Front-to-back iterator over an [`ArrayDeque`] or a [`Deque`](crate::Deque).
#[derive(Clone)]
pub struct Iter<'a, T> {
    inner: core::iter::Chain<slice::Iter<'a, T>, slice::Iter<'a, T>>,
}

impl<'a, T> Iter<'a, T> {
    /// Over the ring's two runs, in order.
    pub(crate) fn new(a: &'a [T], b: &'a [T]) -> Self {
        Self {
            inner: a.iter().chain(b),
        }
    }
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<&'a T> {
        self.inner.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for Iter<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {}

impl<T> FusedIterator for Iter<'_, T> {}

/// Front-to-back mutable iterator over an [`ArrayDeque`] or a [`Deque`](crate::Deque).
pub struct IterMut<'a, T> {
    inner: core::iter::Chain<slice::IterMut<'a, T>, slice::IterMut<'a, T>>,
}

impl<'a, T> IterMut<'a, T> {
    /// Over the ring's two runs, in order.
    pub(crate) fn new(a: &'a mut [T], b: &'a mut [T]) -> Self {
        Self {
            inner: a.iter_mut().chain(b),
        }
    }
}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = &'a mut T;

    #[inline]
    fn next(&mut self) -> Option<&'a mut T> {
        self.inner.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for IterMut<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for IterMut<'_, T> {}

impl<T> FusedIterator for IterMut<'_, T> {}

/// By-value iterator over an [`ArrayDeque`], front to back.
pub struct IntoIter<T, const N: usize> {
    deque: ArrayDeque<T, N>,
}

impl<T, const N: usize> Iterator for IntoIter<T, N> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        self.deque.pop_front()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.deque.len, Some(self.deque.len))
    }
}

impl<T, const N: usize> DoubleEndedIterator for IntoIter<T, N> {
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        self.deque.pop_back()
    }
}

impl<T, const N: usize> ExactSizeIterator for IntoIter<T, N> {}

impl<T, const N: usize> FusedIterator for IntoIter<T, N> {}

impl<T, const N: usize> IntoIterator for ArrayDeque<T, N> {
    type Item = T;
    type IntoIter = IntoIter<T, N>;

    fn into_iter(self) -> IntoIter<T, N> {
        IntoIter { deque: self }
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a ArrayDeque<T, N> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a mut ArrayDeque<T, N> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> IterMut<'a, T> {
        self.iter_mut()
    }
}
