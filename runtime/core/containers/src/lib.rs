//! Hash maps, inline vectors, rings, bit sets, slot maps and sparse sets.
//!
//! - [`HashMap`] / [`HashSet`] / [`HashTable`]: hashbrown with [`marlin_hash::FastState`]. Build
//!   them with [`HashMapExt::new`] (or `Default`), since hashbrown's own `new` needs its default
//!   hasher; [`HashMapInExt::new_in`] for another allocator. Maps keyed by untrusted input take
//!   [`marlin_hash::KeyedState`] instead.
//! - [`IndexMap`] / [`IndexSet`]: hash maps that keep insertion order and index by position.
//! - [`SmallVec`]: up to `N` elements inline, spills to the heap beyond that.
//! - [`ArrayVec`]: at most `N` elements, never allocates.
//! - [`ArrayDeque`]: a ring of at most `N` elements, never allocates. [`Deque`] is the growable
//!   ring (alloc's `VecDeque` takes an allocator only behind an unstable feature).
//! - [`SpscQueue`]: a lock-free single-producer, single-consumer ring, usable in a `static`.
//! - [`BitArray`] / [`BitSet`]: fixed and growable bit sets with set operations.
//! - [`SlotMap`]: stable generational [`Handle`]s; a stale handle returns `None`, never another
//!   element. [`SecondaryMap`] attaches more values to the same handles.
//! - [`SparseSet`]: `u32` keys to densely packed values.
//!
//! Every container that allocates takes an allocator parameter `A` (`core::alloc::Allocator`),
//! defaulting to [`Global`](alloc::alloc::Global), which is `MarlinHeap` in engine binaries.
//! `new` uses the global allocator; `new_in(alloc)` another, e.g. `&marlin_mem::Arena` for
//! per-frame containers. Types with two arrays (`SparseSet`, `IndexMap`) need `A: Clone`.
//!
//! Elsewhere: the bounded multi-producer, multi-consumer queue is `marlin_sync::Channel`, and the
//! fixed-size block pool is `marlin_mem::Pool` / `TypedPool`.
//!
//! No intrusive linked list: a safe one needs pinned nodes and raw back-pointers, and the uses it
//! would serve (free lists, LRU queues) fit an index-linked list over a [`SlotMap`] better.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod array_deque;
mod array_vec;
mod bit_set;
mod deque;
mod hash;
mod index_map;
mod raw;
mod secondary_map;
mod slot_map;
mod small_vec;
mod sparse_set;
mod spsc;

pub use array_deque::ArrayDeque;
pub use array_vec::{ArrayVec, CapacityError};
pub use bit_set::{BitArray, BitSet};
pub use deque::Deque;
pub use hash::{
    HashMap, HashMapExt, HashMapInExt, HashSet, HashSetExt, HashSetInExt, HashTable, hash_map,
    hash_set, hash_table,
};
pub use index_map::{IndexMap, IndexSet};
pub use secondary_map::SecondaryMap;
pub use slot_map::{Handle, SlotMap};
pub use small_vec::SmallVec;
pub use sparse_set::SparseSet;
pub use spsc::{Consumer, Producer, SpscQueue};

/// Iterator types, for naming in signatures.
pub mod iter {
    pub use crate::array_deque::{
        IntoIter as ArrayDequeIntoIter, Iter as DequeIter, IterMut as DequeIterMut,
    };
    pub use crate::array_vec::IntoIter as ArrayVecIntoIter;
    pub use crate::bit_set::Ones;
    pub use crate::deque::IntoIter as DequeIntoIter;
    pub use crate::index_map::{
        IntoIter as IndexMapIntoIter, Iter as IndexMapIter, IterMut as IndexMapIterMut,
    };
    pub use crate::raw::Drain;
    pub use crate::small_vec::IntoIter as SmallVecIntoIter;
}
