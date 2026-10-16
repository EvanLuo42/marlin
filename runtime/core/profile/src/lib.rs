//! CPU profiler for frame analysis.
//!
//! - [`profile_scope!`] opens a zone that ends with the enclosing block. Begin and end are
//!   timestamped with `marlin_std::time` ticks and pushed into the calling thread's lock-free
//!   buffer: no lock and, once the thread's buffer exists, no allocation.
//! - [`frame_mark`] ends a frame: it drains every thread's buffer, pairs begins with ends and
//!   stores the frame in a ring of the last [`history_len`] frames.
//! - [`with_history`] queries that ring: per-zone statistics ([`History::zone_stats`]), a
//!   hierarchical tree per frame ([`History::tree`]) and a Chrome trace export
//!   ([`History::write_chrome_trace`]) that opens in Perfetto and `chrome://tracing`.
//! - [`set_gpu_timings`] is the hook for GPU timings, supplied later by the renderer.
//!
//! In `marlin_build = "shipping"` builds [`profile_scope!`] expands to nothing and
//! [`frame_mark`] does nothing; the query API still compiles and reports no frames.
//!
//! # Zones, fibers and threads
//!
//! Jobs run on fibers that can resume on another thread after a wait, so a zone may begin on one
//! thread and end on another. Each event is recorded on the thread where it happens, and begin
//! and end are paired by a zone id unique for the process. Nesting comes from the order of events
//! in each thread's buffer, so it is exact even when zones begin and end within one clock tick.
//! A zone:
//!
//! - belongs to the thread it began on (`ZoneRecord::thread`; `end_thread` is where it ended);
//! - belongs to the frame whose [`frame_mark`] collects its end. A zone open across a frame mark
//!   carries over and is reported, whole, in the frame where it ends;
//! - is *suspended* if it ended on another thread or overlaps a zone of its thread without
//!   nesting (another job ran on the thread while it waited). Suspended zones count in
//!   statistics with their full duration and appear in trees under the zone that contains them,
//!   but never as a parent, and are exported as async slices since they break the thread's
//!   stack. A zone that waited and resumed on the same thread without such an overlap cannot be
//!   told apart from proper nesting, so zones of other jobs run during the wait show as its
//!   children.
//!
//! A thread's buffer holds [`EVENTS_PER_THREAD`] events between frame marks; events past that
//! are dropped and counted ([`History::dropped_events`]). A zone whose end never arrives is
//! discarded once it is older than the history.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(marlin_build = "shipping", allow(dead_code))]
#![feature(thread_local)]

extern crate alloc;

mod chrome;
mod collector;
mod gpu;
mod record;
mod stats;

pub use chrome::ByteSink;
pub use collector::{
    Frame, History, ThreadInfo, ZoneRecord, clear, frame_index, frame_mark, history_len,
    set_history_len, with_history,
};
pub use gpu::{GpuTimings, GpuZone, set_gpu_timings};
pub use record::{EVENTS_PER_THREAD, set_thread_name};
pub use stats::{FrameTree, Summary, ThreadTree, TreeNode, ZoneStats};

/// Whether zones are recorded: everywhere but shipping builds.
pub const ENABLED: bool = !cfg!(marlin_build = "shipping");

/// Where a zone is declared; one static per [`profile_scope!`].
#[derive(Debug)]
pub struct ZoneSite {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
}

/// An open zone, ended on drop. Made by [`profile_scope!`].
///
/// May be dropped on another thread than the one it began on; see the crate docs.
#[must_use = "the zone ends when this is dropped"]
pub struct Zone {
    /// 0 when nothing was recorded.
    id: u64,
}

impl Zone {
    #[inline]
    pub fn begin(site: &'static ZoneSite) -> Self {
        if !ENABLED {
            return Self { id: 0 };
        }
        Self {
            id: record::begin(site),
        }
    }
}

impl Drop for Zone {
    #[inline]
    fn drop(&mut self) {
        if self.id != 0 {
            record::end(self.id);
        }
    }
}

/// Opens a zone named by a `&'static str` constant until the end of the enclosing block.
///
/// ```ignore
/// fn update() {
///     marlin_profile::profile_scope!("update");
///     // ...
/// }
/// ```
#[cfg(not(marlin_build = "shipping"))]
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        let _marlin_profile_zone = {
            static SITE: $crate::ZoneSite = $crate::ZoneSite {
                name: $name,
                file: ::core::file!(),
                line: ::core::line!(),
            };
            $crate::Zone::begin(&SITE)
        };
    };
}

/// Shipping builds record nothing.
#[cfg(marlin_build = "shipping")]
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {};
}
