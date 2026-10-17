//! `Deque<T, A>`: a growable ring buffer in an allocator.

use alloc::alloc::{Allocator, Global};
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::mem::{MaybeUninit, size_of};
use core::ops::{Index, IndexMut};
use core::{cmp, fmt, ptr, slice};

use crate::array_deque::{Iter, IterMut};

/// A double-ended queue that grows as needed, in one block from `A`: what alloc's `VecDeque`
/// does, which takes an allocator only behind an unstable feature.
///
/// Push and pop at either end are O(1) amortized; growing moves only the wrapped-around part.
/// [`ArrayDeque`](crate::ArrayDeque) is the fixed-capacity, non-allocating version.
pub struct Deque<T, A: Allocator = Global> {
    /// The storage: `buf.len() == buf.capacity()` always, as uninitialized slots (unused for
    /// zero-sized `T`).
    buf: Vec<MaybeUninit<T>, A>,
    /// Physical index of the front element.
    head: usize,
    len: usize,
}

impl<T> Deque<T> {
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
            len: 0,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_in(capacity, Global)
    }
}

impl<T, A: Allocator> Deque<T, A> {
    pub fn new_in(alloc: A) -> Self {
        Self::with_capacity_in(0, alloc)
    }

    pub fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        let mut d = Self {
            buf: Vec::new_in(alloc),
            head: 0,
            len: 0,
        };
        d.reserve(capacity);
        d
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.buf.allocator()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Elements that fit without reallocating.
    #[inline]
    pub fn capacity(&self) -> usize {
        if size_of::<T>() == 0 {
            usize::MAX
        } else {
            self.buf.len()
        }
    }

    /// The physical slot of logical index `i < capacity`. Zero-sized elements all live at 0.
    #[inline(always)]
    fn slot(&self, i: usize) -> usize {
        if size_of::<T>() == 0 {
            return 0;
        }
        let cap = self.buf.len();
        // head < cap and i < cap, so this cannot overflow on any real allocation.
        let p = self.head + i;
        if p >= cap { p - cap } else { p }
    }

    #[inline(always)]
    fn base(&mut self) -> *mut T {
        self.buf.as_mut_ptr().cast()
    }

    /// Makes room for at least `additional` more elements.
    pub fn reserve(&mut self, additional: usize) {
        let required = self
            .len
            .checked_add(additional)
            .expect("Deque capacity overflow");
        if required <= self.capacity() {
            return;
        }
        let old_cap = self.buf.len();
        let new_cap = cmp::max(cmp::max(required, old_cap.saturating_mul(2)), 4);
        self.buf.reserve_exact(new_cap - old_cap);
        let new_cap = self.buf.capacity();
        // SAFETY: the slots are `MaybeUninit`, valid uninitialized; the old ones were preserved.
        unsafe { self.buf.set_len(new_cap) };
        if self.head + self.len > old_cap {
            // Wrapped: [b..][free][a..] with `a` the front run. Move whichever run fits.
            let front = old_cap - self.head;
            let back = self.len - front;
            let base = self.base();
            // SAFETY: both runs are initialized; the targets lie in the new capacity and are
            // free; `copy` allows the overlap of the second case.
            unsafe {
                if back <= new_cap - old_cap {
                    ptr::copy_nonoverlapping(base, base.add(old_cap), back);
                } else {
                    let new_head = new_cap - front;
                    ptr::copy(base.add(self.head), base.add(new_head), front);
                    self.head = new_head;
                }
            }
        }
    }

    pub fn push_back(&mut self, value: T) {
        if self.len == self.capacity() {
            self.reserve(1);
        }
        let slot = self.slot(self.len);
        // SAFETY: len < capacity, so the slot is free.
        unsafe { self.base().add(slot).write(value) };
        self.len += 1;
    }

    pub fn push_front(&mut self, value: T) {
        if self.len == self.capacity() {
            self.reserve(1);
        }
        if size_of::<T>() != 0 {
            self.head = if self.head == 0 {
                self.buf.len() - 1
            } else {
                self.head - 1
            };
        }
        let head = self.head;
        // SAFETY: len < capacity, so the slot before the old head is free.
        unsafe { self.base().add(head).write(value) };
        self.len += 1;
    }

    pub fn pop_front(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let head = self.head;
        self.head = self.slot(1);
        self.len -= 1;
        if self.len == 0 {
            self.head = 0;
        }
        // SAFETY: the old front was initialized and is no longer counted.
        Some(unsafe { self.base().add(head).read() })
    }

    pub fn pop_back(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        let slot = self.slot(self.len);
        // SAFETY: the old back was initialized and is no longer counted.
        Some(unsafe { self.base().add(slot).read() })
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        // SAFETY: index < len, so the slot is initialized.
        Some(unsafe { &*self.buf.as_ptr().cast::<T>().add(self.slot(index)) })
    }

    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        let slot = self.slot(index);
        // SAFETY: index < len, so the slot is initialized.
        Some(unsafe { &mut *self.base().add(slot) })
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

    /// The run lengths: up to the end of the buffer, then from its start.
    #[inline]
    fn runs(&self) -> (usize, usize) {
        if size_of::<T>() == 0 {
            return (self.len, 0);
        }
        let first = self.len.min(self.buf.len() - self.head);
        (first, self.len - first)
    }

    /// The elements in order, as the run up to the end of the buffer and the wrapped-around rest.
    pub fn as_slices(&self) -> (&[T], &[T]) {
        let (first, rest) = self.runs();
        let base = self.buf.as_ptr().cast::<T>();
        // SAFETY: both runs are initialized: `head..head + first` and `0..rest`.
        unsafe {
            (
                slice::from_raw_parts(base.add(self.head), first),
                slice::from_raw_parts(base, rest),
            )
        }
    }

    /// [`as_slices`](Self::as_slices), mutably.
    pub fn as_mut_slices(&mut self) -> (&mut [T], &mut [T]) {
        let (first, rest) = self.runs();
        let head = self.head;
        let base = self.base();
        // SAFETY: the runs are initialized and disjoint.
        unsafe {
            (
                slice::from_raw_parts_mut(base.add(head), first),
                slice::from_raw_parts_mut(base, rest),
            )
        }
    }

    /// Moves the elements so they are contiguous from slot 0 and returns them as one slice.
    pub fn make_contiguous(&mut self) -> &mut [T] {
        if size_of::<T>() != 0 {
            if self.head + self.len > self.buf.len() {
                // [back run][free][front run]: rotating the raw slots puts the front run first.
                let head = self.head;
                self.buf.rotate_left(head);
            } else if self.head != 0 && self.len > 0 {
                let (head, len) = (self.head, self.len);
                let base = self.base();
                // SAFETY: the run is initialized and inside the buffer; `copy` allows the overlap.
                unsafe { ptr::copy(base.add(head), base, len) };
            }
        }
        self.head = 0;
        let len = self.len;
        // SAFETY: the `len` elements now start at slot 0.
        unsafe { slice::from_raw_parts_mut(self.base(), len) }
    }

    /// Drops the elements from `len` on.
    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            drop(self.pop_back());
        }
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

impl<T, A: Allocator> Drop for Deque<T, A> {
    fn drop(&mut self) {
        // `buf` then frees the block; its `MaybeUninit` slots drop nothing.
        self.clear();
    }
}

impl<T> Default for Deque<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone, A: Allocator + Clone> Clone for Deque<T, A> {
    fn clone(&self) -> Self {
        let mut d = Self::with_capacity_in(self.len, self.allocator().clone());
        d.extend(self.iter().cloned());
        d
    }
}

impl<T: fmt::Debug, A: Allocator> fmt::Debug for Deque<T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: PartialEq, A: Allocator, B: Allocator> PartialEq<Deque<T, B>> for Deque<T, A> {
    fn eq(&self, other: &Deque<T, B>) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq, A: Allocator> Eq for Deque<T, A> {}

impl<T: Hash, A: Allocator> Hash for Deque<T, A> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len);
        for x in self {
            x.hash(state);
        }
    }
}

impl<T, A: Allocator> Index<usize> for Deque<T, A> {
    type Output = T;

    #[track_caller]
    fn index(&self, index: usize) -> &T {
        match self.get(index) {
            Some(x) => x,
            None => panic!("index {index} out of bounds (len {})", self.len),
        }
    }
}

impl<T, A: Allocator> IndexMut<usize> for Deque<T, A> {
    #[track_caller]
    fn index_mut(&mut self, index: usize) -> &mut T {
        let len = self.len;
        match self.get_mut(index) {
            Some(x) => x,
            None => panic!("index {index} out of bounds (len {len})"),
        }
    }
}

impl<T, A: Allocator> Extend<T> for Deque<T, A> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for x in iter {
            self.push_back(x);
        }
    }
}

impl<T> FromIterator<T> for Deque<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut d = Self::new();
        d.extend(iter);
        d
    }
}

/// By-value iterator over a [`Deque`], front to back.
pub struct IntoIter<T, A: Allocator = Global> {
    deque: Deque<T, A>,
}

impl<T, A: Allocator> Iterator for IntoIter<T, A> {
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

impl<T, A: Allocator> DoubleEndedIterator for IntoIter<T, A> {
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        self.deque.pop_back()
    }
}

impl<T, A: Allocator> ExactSizeIterator for IntoIter<T, A> {}

impl<T, A: Allocator> FusedIterator for IntoIter<T, A> {}

impl<T, A: Allocator> IntoIterator for Deque<T, A> {
    type Item = T;
    type IntoIter = IntoIter<T, A>;

    fn into_iter(self) -> IntoIter<T, A> {
        IntoIter { deque: self }
    }
}

impl<'a, T, A: Allocator> IntoIterator for &'a Deque<T, A> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<'a, T, A: Allocator> IntoIterator for &'a mut Deque<T, A> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> IterMut<'a, T> {
        self.iter_mut()
    }
}
