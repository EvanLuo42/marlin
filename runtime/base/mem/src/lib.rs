//! Memory: page allocation, arenas, pools, the general-purpose heap and the global allocator.
//!
//! - [`MarlinHeap`]: the general heap, as `GlobalAlloc` and as an `Allocator` handle; [`stats`],
//!   [`purge`] and [`set_purge_delay`] manage what it holds from the OS.
//! - [`Arena`] / [`FrameArenas`]: linear allocation, freed all at once.
//! - [`Pool`] / [`TypedPool`]: fixed-size blocks.
//! - [`tag`] and [`budget`]: attribution of memory to subsystems, and soft limits.
//! - [`vmem`]: virtual memory for crates that may not use the PAL directly.
//! - `debug` (feature `mem-debug`): guard pages and leak tracking for the heap.
//! - [`ext`]: large-page arenas and mirrored rings, where the platform has them.
//!
//! This crate implements `alloc`, so it must not use it.
#![cfg_attr(not(test), no_std)]
#![feature(thread_local)]

mod alloc_api;
pub mod arena;
pub mod budget;
#[cfg(feature = "mem-debug")]
pub mod debug;
pub mod ext;
mod global;
mod heap;
mod layout;
mod list;
mod lock;
pub mod pool;
mod segment;
pub mod tag;
mod vrange;

/// Virtual memory: reserve address space, commit and decommit pages of it.
pub mod vmem {
    pub use marlin_pal::Protection;
    pub use marlin_pal::vmem::{allocation_granularity, page_size};

    pub use crate::vrange::Reservation;
}

pub use arena::{Arena, FrameArenas};
pub use budget::{MemoryInfo, budget, is_over_budget, set_budget, system_memory};
pub use global::MarlinHeap;
pub use heap::{
    DEFAULT_PURGE_DELAY, HeapStats, collect_abandoned, good_size, purge, purge_delay,
    set_purge_delay, stats,
};
pub use pool::{Pool, TypedPool};
pub use tag::{MemTag, TagStats, Tagged, tag_stats};
