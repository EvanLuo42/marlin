//! Memory: page allocation, arenas, the general-purpose heap and the global allocator.
//!
//! This crate implements `alloc`, so it must not use it.
#![cfg_attr(not(test), no_std)]

mod global;

pub use global::MarlinHeap;
