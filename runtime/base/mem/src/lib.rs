//! Memory: page allocation, arenas, the general-purpose heap and the global allocator.
//!
//! This crate implements `alloc`, so it must not use it.
#![cfg_attr(not(test), no_std)]
#![feature(thread_local)]

mod global;
mod heap;
mod layout;
mod list;
mod lock;
mod segment;

pub use global::MarlinHeap;
pub use heap::{HeapStats, collect_abandoned, good_size, stats};
