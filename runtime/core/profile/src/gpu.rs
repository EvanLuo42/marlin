//! The hook through which the renderer supplies GPU timings.

use core::time::Duration;

use crate::collector::{self, ticks_to_duration};

/// A GPU zone, already converted to the CPU clock (`marlin_std::time` ticks) by the renderer,
/// which owns the GPU/CPU clock calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuZone {
    /// The CPU frame that submitted the work: [`frame_index`](crate::frame_index) at submission.
    pub frame: u64,
    pub name: &'static str,
    /// The queue, numbered by the source; see [`GpuTimings::queue_name`].
    pub queue: u32,
    pub start: u64,
    pub end: u64,
}

impl GpuZone {
    pub fn duration(&self) -> Duration {
        ticks_to_duration(self.end.saturating_sub(self.start))
    }
}

/// A source of resolved GPU timings, typically the renderer's timestamp queries.
pub trait GpuTimings: Sync {
    /// Called by every [`frame_mark`](crate::frame_mark) with the profiler locked, so it must not
    /// call back into the profiler. Reports the zones resolved since the last call; they usually
    /// lag a few frames, and those of frames already out of the history are dropped.
    fn collect(&self, out: &mut dyn FnMut(GpuZone));

    /// Shown as the queue's track name in traces.
    fn queue_name(&self, queue: u32) -> &str {
        let _ = queue;
        "GPU"
    }
}

/// Installs (or with `None` removes) the GPU timing source.
pub fn set_gpu_timings(source: Option<&'static dyn GpuTimings>) {
    collector::set_gpu(source);
}
