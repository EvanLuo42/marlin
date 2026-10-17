//! Per-tag memory budgets.
//!
//! A budget is a soft limit on a tag's [bytes](crate::TagStats::bytes): nothing fails when it is
//! exceeded. Systems that can shed memory poll [`is_over_budget`] (one cache line, two relaxed
//! loads) and react, e.g. streaming stops prefetching and drops cached mips; development builds
//! can report the overruns.
//!
//! [`system_memory`] reports the totals budgets are usually derived from: physical memory, the
//! process's resident set and private memory.

use core::sync::atomic::Ordering;

use crate::tag::{MemTag, counters};

/// Sets `tag`'s budget in bytes, or removes it with `None`.
pub fn set_budget(tag: MemTag, bytes: Option<usize>) {
    // `usize::MAX` bytes cannot be exceeded, so it doubles as "no budget".
    let budget = bytes.unwrap_or(usize::MAX);
    counters(tag).budget.store(budget, Ordering::Relaxed);
}

pub fn budget(tag: MemTag) -> Option<usize> {
    match counters(tag).budget.load(Ordering::Relaxed) {
        usize::MAX => None,
        bytes => Some(bytes),
    }
}

/// Whether `tag` holds more bytes than its budget allows (never, without a budget).
#[inline]
pub fn is_over_budget(tag: MemTag) -> bool {
    let c = counters(tag);
    c.bytes.load(Ordering::Relaxed) > c.budget.load(Ordering::Relaxed)
}

/// Bytes `tag` may still allocate before it is over budget; `None` without a budget.
pub fn budget_remaining(tag: MemTag) -> Option<usize> {
    let c = counters(tag);
    budget(tag).map(|b| b.saturating_sub(c.bytes.load(Ordering::Relaxed)))
}

/// Calls `f` with every tag over its budget, its bytes and its budget.
pub fn for_each_over_budget(mut f: impl FnMut(MemTag, usize, usize)) {
    for tag in MemTag::ALL {
        let c = counters(tag);
        let (bytes, budget) = (
            c.bytes.load(Ordering::Relaxed),
            c.budget.load(Ordering::Relaxed),
        );
        if bytes > budget {
            f(tag, bytes, budget);
        }
    }
}

pub use marlin_pal::sysinfo::MemoryInfo;

/// The system's and this process's memory (see [`MemoryInfo`] for what each platform reports).
/// A few system calls; poll it at most once a frame.
pub fn system_memory() -> marlin_pal::Result<MemoryInfo> {
    marlin_pal::sysinfo::memory()
}
