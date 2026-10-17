//! Bit sets of small integers: [`BitArray`] (fixed size, inline) and [`BitSet`] (growable).
//!
//! Both store bit `i` in word `i / 64`, bit `i % 64`; iteration yields the set bits in ascending
//! order, a word at a time.

use alloc::alloc::{Allocator, Global};
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::ops::{
    BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not, Sub, SubAssign,
};
use core::{fmt, slice};

const BITS: usize = u64::BITS as usize;

#[inline(always)]
const fn split(i: usize) -> (usize, u64) {
    (i / BITS, 1 << (i % BITS))
}

fn count(words: &[u64]) -> usize {
    words.iter().map(|w| w.count_ones() as usize).sum()
}

fn first(words: &[u64]) -> Option<usize> {
    let (i, w) = words.iter().enumerate().find(|(_, w)| **w != 0)?;
    Some(i * BITS + w.trailing_zeros() as usize)
}

fn last(words: &[u64]) -> Option<usize> {
    let (i, w) = words.iter().enumerate().rfind(|(_, w)| **w != 0)?;
    Some(i * BITS + (BITS - 1 - w.leading_zeros() as usize))
}

/// `a ⊆ b`, where missing words are zero.
fn subset(a: &[u64], b: &[u64]) -> bool {
    a.iter()
        .enumerate()
        .all(|(i, &w)| w & !b.get(i).copied().unwrap_or(0) == 0)
}

fn disjoint(a: &[u64], b: &[u64]) -> bool {
    a.iter().zip(b).all(|(x, y)| x & y == 0)
}

/// `words` without its trailing zero words: the part that equality and hashing see.
fn significant(words: &[u64]) -> &[u64] {
    let n = words.iter().rposition(|&w| w != 0).map_or(0, |i| i + 1);
    &words[..n]
}

/// The set bits of a word slice, ascending.
#[derive(Clone)]
pub struct Ones<'a> {
    words: slice::Iter<'a, u64>,
    /// Bit index of `current`'s bit 0.
    base: usize,
    current: u64,
}

impl<'a> Ones<'a> {
    fn new(words: &'a [u64]) -> Self {
        let mut words = words.iter();
        let current = words.next().copied().unwrap_or(0);
        Self {
            words,
            base: 0,
            current,
        }
    }
}

impl Iterator for Ones<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        while self.current == 0 {
            self.current = *self.words.next()?;
            self.base += BITS;
        }
        let bit = self.current.trailing_zeros() as usize;
        self.current &= self.current - 1;
        Some(self.base + bit)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.current.count_ones() as usize + count(self.words.as_slice());
        (n, Some(n))
    }
}

impl ExactSizeIterator for Ones<'_> {}

impl FusedIterator for Ones<'_> {}

impl fmt::Debug for Ones<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

// --- BitArray ---

/// A fixed set of `W * 64` bits, stored inline: `BitArray<4>` holds bits `0..256`. (The word
/// count is the parameter because stable const generics cannot compute `bits.div_ceil(64)`.)
///
/// `insert` panics past the end; `contains` and `remove` just miss. Set operations are word-wise
/// and also available as operators: `|` union, `&` intersection, `^` symmetric difference, `-`
/// difference, `!` complement.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BitArray<const W: usize> {
    words: [u64; W],
}

impl<const W: usize> BitArray<W> {
    /// The number of bits, `W * 64`.
    pub const BITS: usize = W * BITS;

    pub const fn new() -> Self {
        Self { words: [0; W] }
    }

    /// Every bit set.
    pub const fn full() -> Self {
        Self { words: [!0; W] }
    }

    #[inline]
    pub const fn from_words(words: [u64; W]) -> Self {
        Self { words }
    }

    #[inline]
    pub const fn words(&self) -> &[u64; W] {
        &self.words
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        Self::BITS
    }

    #[inline]
    pub const fn contains(&self, i: usize) -> bool {
        let (w, m) = split(i);
        w < W && self.words[w] & m != 0
    }

    /// Sets bit `i`; returns whether it was clear. Panics if `i >= BITS`.
    #[inline]
    #[track_caller]
    pub const fn insert(&mut self, i: usize) -> bool {
        assert!(i < Self::BITS, "bit index out of range");
        let (w, m) = split(i);
        let was = self.words[w] & m;
        self.words[w] |= m;
        was == 0
    }

    /// Clears bit `i`; returns whether it was set.
    #[inline]
    pub const fn remove(&mut self, i: usize) -> bool {
        let (w, m) = split(i);
        if w >= W {
            return false;
        }
        let was = self.words[w] & m;
        self.words[w] &= !m;
        was != 0
    }

    /// Sets or clears bit `i`. Panics if setting past the end.
    #[inline]
    #[track_caller]
    pub const fn set(&mut self, i: usize, value: bool) {
        if value {
            self.insert(i);
        } else {
            self.remove(i);
        }
    }

    /// Flips bit `i`. Panics if `i >= BITS`.
    #[inline]
    #[track_caller]
    pub const fn toggle(&mut self, i: usize) {
        assert!(i < Self::BITS, "bit index out of range");
        let (w, m) = split(i);
        self.words[w] ^= m;
    }

    /// The number of set bits.
    pub const fn len(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < W {
            n += self.words[i].count_ones() as usize;
            i += 1;
        }
        n
    }

    pub const fn is_empty(&self) -> bool {
        let mut i = 0;
        while i < W {
            if self.words[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    pub fn clear(&mut self) {
        self.words = [0; W];
    }

    /// The set bits, ascending.
    #[inline]
    pub fn iter(&self) -> Ones<'_> {
        Ones::new(&self.words)
    }

    /// The lowest set bit.
    #[inline]
    pub fn first(&self) -> Option<usize> {
        first(&self.words)
    }

    /// The highest set bit.
    #[inline]
    pub fn last(&self) -> Option<usize> {
        last(&self.words)
    }

    pub fn union_with(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= b;
        }
    }

    pub fn intersect_with(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= b;
        }
    }

    pub fn difference_with(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= !b;
        }
    }

    pub fn symmetric_difference_with(&mut self, other: &Self) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a ^= b;
        }
    }

    pub fn is_subset(&self, other: &Self) -> bool {
        subset(&self.words, &other.words)
    }

    pub fn is_superset(&self, other: &Self) -> bool {
        other.is_subset(self)
    }

    pub fn is_disjoint(&self, other: &Self) -> bool {
        disjoint(&self.words, &other.words)
    }
}

impl<const W: usize> Default for BitArray<W> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> fmt::Debug for BitArray<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<'a, const W: usize> IntoIterator for &'a BitArray<W> {
    type Item = usize;
    type IntoIter = Ones<'a>;

    fn into_iter(self) -> Ones<'a> {
        self.iter()
    }
}

impl<const W: usize> Extend<usize> for BitArray<W> {
    /// Panics on a bit past the end.
    #[track_caller]
    fn extend<I: IntoIterator<Item = usize>>(&mut self, iter: I) {
        for i in iter {
            self.insert(i);
        }
    }
}

impl<const W: usize> FromIterator<usize> for BitArray<W> {
    /// Panics on a bit past the end.
    #[track_caller]
    fn from_iter<I: IntoIterator<Item = usize>>(iter: I) -> Self {
        let mut s = Self::new();
        s.extend(iter);
        s
    }
}

macro_rules! array_ops {
    ($($op:ident $fn:ident $assign:ident $assign_fn:ident $with:ident),*) => {$(
        impl<const W: usize> $op for BitArray<W> {
            type Output = Self;

            #[inline]
            fn $fn(mut self, rhs: Self) -> Self {
                self.$with(&rhs);
                self
            }
        }

        impl<const W: usize> $assign for BitArray<W> {
            #[inline]
            fn $assign_fn(&mut self, rhs: Self) {
                self.$with(&rhs);
            }
        }
    )*};
}

array_ops! {
    BitOr bitor BitOrAssign bitor_assign union_with,
    BitAnd bitand BitAndAssign bitand_assign intersect_with,
    BitXor bitxor BitXorAssign bitxor_assign symmetric_difference_with,
    Sub sub SubAssign sub_assign difference_with
}

impl<const W: usize> Not for BitArray<W> {
    type Output = Self;

    #[inline]
    fn not(mut self) -> Self {
        for w in &mut self.words {
            *w = !*w;
        }
        self
    }
}

// --- BitSet ---

/// A growable set of small integers: one bit each, in words from `A`. `insert` grows to fit;
/// lookups past the end miss. Memory is proportional to the largest element, so this suits dense
/// ids (entity indices, node ids), not sparse large ones.
///
/// Equality and hashing ignore unused capacity: two sets with the same bits are equal however
/// they grew. Operators work on references: `&a | &b`, `a |= &b`.
pub struct BitSet<A: Allocator = Global> {
    words: Vec<u64, A>,
}

impl BitSet {
    pub const fn new() -> Self {
        Self { words: Vec::new() }
    }

    /// Room for bits `0..bits` without growing.
    pub fn with_capacity(bits: usize) -> Self {
        Self::with_capacity_in(bits, Global)
    }
}

impl<A: Allocator> BitSet<A> {
    pub fn new_in(alloc: A) -> Self {
        Self {
            words: Vec::new_in(alloc),
        }
    }

    pub fn with_capacity_in(bits: usize, alloc: A) -> Self {
        let mut words = Vec::with_capacity_in(bits.div_ceil(BITS), alloc);
        words.resize(bits.div_ceil(BITS), 0);
        Self { words }
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.words.allocator()
    }

    /// Bits addressable without growing.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.words.len() * BITS
    }

    /// The words, bit `i` in `words()[i / 64]`; may end in zero words.
    #[inline]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    #[inline]
    pub fn contains(&self, i: usize) -> bool {
        let (w, m) = split(i);
        self.words.get(w).is_some_and(|&x| x & m != 0)
    }

    /// Sets bit `i`, growing if needed; returns whether it was clear.
    pub fn insert(&mut self, i: usize) -> bool {
        let (w, m) = split(i);
        if w >= self.words.len() {
            self.words.resize(w + 1, 0);
        }
        let was = self.words[w] & m;
        self.words[w] |= m;
        was == 0
    }

    /// Clears bit `i`; returns whether it was set.
    pub fn remove(&mut self, i: usize) -> bool {
        let (w, m) = split(i);
        match self.words.get_mut(w) {
            Some(x) => {
                let was = *x & m;
                *x &= !m;
                was != 0
            }
            None => false,
        }
    }

    pub fn set(&mut self, i: usize, value: bool) {
        if value {
            self.insert(i);
        } else {
            self.remove(i);
        }
    }

    /// Flips bit `i`, growing if needed.
    pub fn toggle(&mut self, i: usize) {
        if !self.insert(i) {
            self.remove(i);
        }
    }

    /// The number of set bits.
    #[inline]
    pub fn len(&self) -> usize {
        count(&self.words)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Clears every bit; keeps the memory.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// Releases the words past the highest set bit.
    pub fn shrink_to_fit(&mut self) {
        let n = significant(&self.words).len();
        self.words.truncate(n);
        self.words.shrink_to_fit();
    }

    /// The set bits, ascending.
    #[inline]
    pub fn iter(&self) -> Ones<'_> {
        Ones::new(&self.words)
    }

    #[inline]
    pub fn first(&self) -> Option<usize> {
        first(&self.words)
    }

    #[inline]
    pub fn last(&self) -> Option<usize> {
        last(&self.words)
    }

    pub fn union_with<B: Allocator>(&mut self, other: &BitSet<B>) {
        let other = significant(&other.words);
        if other.len() > self.words.len() {
            self.words.resize(other.len(), 0);
        }
        for (a, b) in self.words.iter_mut().zip(other) {
            *a |= b;
        }
    }

    pub fn intersect_with<B: Allocator>(&mut self, other: &BitSet<B>) {
        let n = other.words.len().min(self.words.len());
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= b;
        }
        self.words[n..].fill(0);
    }

    pub fn difference_with<B: Allocator>(&mut self, other: &BitSet<B>) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= !b;
        }
    }

    pub fn symmetric_difference_with<B: Allocator>(&mut self, other: &BitSet<B>) {
        let other = significant(&other.words);
        if other.len() > self.words.len() {
            self.words.resize(other.len(), 0);
        }
        for (a, b) in self.words.iter_mut().zip(other) {
            *a ^= b;
        }
    }

    pub fn is_subset<B: Allocator>(&self, other: &BitSet<B>) -> bool {
        subset(&self.words, &other.words)
    }

    pub fn is_superset<B: Allocator>(&self, other: &BitSet<B>) -> bool {
        other.is_subset(self)
    }

    pub fn is_disjoint<B: Allocator>(&self, other: &BitSet<B>) -> bool {
        disjoint(&self.words, &other.words)
    }
}

impl Default for BitSet {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator + Clone> Clone for BitSet<A> {
    fn clone(&self) -> Self {
        Self {
            words: self.words.clone(),
        }
    }
}

impl<A: Allocator, B: Allocator> PartialEq<BitSet<B>> for BitSet<A> {
    fn eq(&self, other: &BitSet<B>) -> bool {
        significant(&self.words) == significant(&other.words)
    }
}

impl<A: Allocator> Eq for BitSet<A> {}

impl<A: Allocator> Hash for BitSet<A> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        significant(&self.words).hash(state);
    }
}

impl<A: Allocator> fmt::Debug for BitSet<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<'a, A: Allocator> IntoIterator for &'a BitSet<A> {
    type Item = usize;
    type IntoIter = Ones<'a>;

    fn into_iter(self) -> Ones<'a> {
        self.iter()
    }
}

impl<A: Allocator> Extend<usize> for BitSet<A> {
    fn extend<I: IntoIterator<Item = usize>>(&mut self, iter: I) {
        for i in iter {
            self.insert(i);
        }
    }
}

impl FromIterator<usize> for BitSet {
    fn from_iter<I: IntoIterator<Item = usize>>(iter: I) -> Self {
        let mut s = Self::new();
        s.extend(iter);
        s
    }
}

impl<const W: usize> From<BitArray<W>> for BitSet {
    fn from(a: BitArray<W>) -> Self {
        Self {
            words: a.words.to_vec(),
        }
    }
}

macro_rules! set_ops {
    ($($op:ident $fn:ident $assign:ident $assign_fn:ident $with:ident),*) => {$(
        impl<A: Allocator + Clone, B: Allocator> $op<&BitSet<B>> for &BitSet<A> {
            type Output = BitSet<A>;

            #[inline]
            fn $fn(self, rhs: &BitSet<B>) -> BitSet<A> {
                let mut out = self.clone();
                out.$with(rhs);
                out
            }
        }

        impl<A: Allocator, B: Allocator> $assign<&BitSet<B>> for BitSet<A> {
            #[inline]
            fn $assign_fn(&mut self, rhs: &BitSet<B>) {
                self.$with(rhs);
            }
        }
    )*};
}

set_ops! {
    BitOr bitor BitOrAssign bitor_assign union_with,
    BitAnd bitand BitAndAssign bitand_assign intersect_with,
    BitXor bitxor BitXorAssign bitxor_assign symmetric_difference_with,
    Sub sub SubAssign sub_assign difference_with
}
