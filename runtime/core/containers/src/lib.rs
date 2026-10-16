//! Hash maps, inline vectors, slot maps and sparse sets.
//!
//! - [`HashMap`] / [`HashSet`] / [`HashTable`]: hashbrown with [`marlin_hash::FastState`]. Build
//!   them with [`HashMapExt::new`] (or `Default`), since hashbrown's own `new` needs its default
//!   hasher.
//! - [`SmallVec`]: up to `N` elements inline, spills to the heap beyond that.
//! - [`ArrayVec`]: at most `N` elements, never allocates.
//! - [`SlotMap`]: stable generational [`Handle`]s; a stale handle returns `None`, never another
//!   element.
//! - [`SparseSet`]: `u32` keys to densely packed values.
//!
//! Everything allocates from the global allocator (`MarlinHeap` in engine binaries); there is no
//! `allocator_api` parameter yet.
//!
//! No intrusive linked list: a safe one needs pinned nodes and raw back-pointers, and the uses it
//! would serve (free lists, LRU queues) fit an index-linked list over a [`SlotMap`] better.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod array_vec;
mod hash;
mod raw;
mod slot_map;
mod small_vec;
mod sparse_set;

pub use array_vec::{ArrayVec, CapacityError};
pub use hash::{
    HashMap, HashMapExt, HashSet, HashSetExt, HashTable, hash_map, hash_set, hash_table,
};
pub use slot_map::{Handle, SlotMap};
pub use small_vec::SmallVec;
pub use sparse_set::SparseSet;

/// Iterator types, for naming in signatures.
pub mod iter {
    pub use crate::array_vec::IntoIter as ArrayVecIntoIter;
    pub use crate::small_vec::IntoIter as SmallVecIntoIter;
}
