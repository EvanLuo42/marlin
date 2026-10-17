//! Query pools (timestamps, occlusion, pipeline statistics), predication, clock calibration and
//! buffer markers.

use alloc::sync::Arc;

use super::command::CommandList;
use super::device::{Device, Shared};
use super::ffi::*;
use super::queue::Queue;
use crate::{ClockCalibration, MarkerPoint, PredicateSkip, QueryPoolDesc, Result};

/// Recording state of the query area, reset at every `begin`.
#[derive(Default)]
pub(crate) struct QueryState {}

pub(crate) struct QueryPool {
    shared: Arc<Shared>,
    raw: VkQueryPool,
}

impl QueryPool {
    pub(crate) fn raw(&self) -> VkQueryPool {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

pub(crate) fn create_query_pool(device: &Device, desc: &QueryPoolDesc) -> Result<QueryPool> {
    let _ = (device, desc);
    todo!("vulkan: create_query_pool")
}

pub(crate) fn clock_calibration(queue: &Queue) -> Result<ClockCalibration> {
    let _ = queue;
    todo!("vulkan: clock_calibration")
}

pub(crate) fn write_timestamp(list: &mut CommandList, pool: &QueryPool, index: u32) {
    let _ = (list, pool, index);
    todo!("vulkan: write_timestamp")
}

pub(crate) fn begin_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
    let _ = (list, pool, index);
    todo!("vulkan: begin_query")
}

pub(crate) fn end_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
    let _ = (list, pool, index);
    todo!("vulkan: end_query")
}

pub(crate) fn resolve_queries(
    list: &mut CommandList,
    pool: &QueryPool,
    first: u32,
    count: u32,
    dst: &crate::Buffer,
    offset: u64,
) {
    let _ = (list, pool, first, count, dst, offset);
    todo!("vulkan: resolve_queries")
}

pub(crate) fn set_predication(
    list: &mut CommandList,
    predicate: Option<(&crate::Buffer, u64, PredicateSkip)>,
) {
    let _ = (list, predicate);
    todo!("vulkan: set_predication")
}

pub(crate) fn write_marker(
    list: &mut CommandList,
    buffer: &crate::Buffer,
    offset: u64,
    value: u32,
    point: MarkerPoint,
) {
    let _ = (list, buffer, offset, value, point);
    todo!("vulkan: write_marker")
}
