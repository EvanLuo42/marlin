//! Query pools (timestamps, occlusion, pipeline statistics), predication, clock calibration and
//! buffer markers.
//!
//! Vulkan queries must be reset before each use, which D3D12's do not need. Pools are reset on the
//! host when created, and `resolve_queries` resets the queries it reads right after copying them:
//! write (or begin/end) a query, resolve it once, and it is ready for the next frame. Resolving a
//! query that was not written since its last resolve waits for a result that never comes (a GPU
//! hang), so the caller resolves only what it wrote.

use core::ptr;

use alloc::sync::Arc;

use super::command::CommandList;
use super::device::{Device, Shared};
use super::ffi::*;
use super::queue::Queue;
use super::unsupported;
use crate::{ClockCalibration, MarkerPoint, PredicateSkip, QueryKind, QueryPoolDesc, Result};

/// Recording state of the query area, reset at every `begin`.
#[derive(Default)]
pub(crate) struct QueryState {
    /// Conditional rendering is active (`set_predication`).
    predicating: bool,
}

/// Every statistic the RHI's `PipelineStatistics` has but the mesh primitive count, in its field
/// order (which is Vulkan's bit order, so the results come out in the struct's layout).
const ALL_STATISTICS: VkQueryPipelineStatisticFlags =
    VK_QUERY_PIPELINE_STATISTIC_INPUT_ASSEMBLY_VERTICES_BIT
        | VK_QUERY_PIPELINE_STATISTIC_INPUT_ASSEMBLY_PRIMITIVES_BIT
        | VK_QUERY_PIPELINE_STATISTIC_VERTEX_SHADER_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_GEOMETRY_SHADER_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_GEOMETRY_SHADER_PRIMITIVES_BIT
        | VK_QUERY_PIPELINE_STATISTIC_CLIPPING_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_CLIPPING_PRIMITIVES_BIT
        | VK_QUERY_PIPELINE_STATISTIC_FRAGMENT_SHADER_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_TESSELLATION_CONTROL_SHADER_PATCHES_BIT
        | VK_QUERY_PIPELINE_STATISTIC_TESSELLATION_EVALUATION_SHADER_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_COMPUTE_SHADER_INVOCATIONS_BIT
        | VK_QUERY_PIPELINE_STATISTIC_TASK_SHADER_INVOCATIONS_BIT_EXT
        | VK_QUERY_PIPELINE_STATISTIC_MESH_SHADER_INVOCATIONS_BIT_EXT;

/// Byte offsets in one `PipelineStatistics` record.
const COMPUTE_INVOCATIONS_OFFSET: u64 = 10 * 8;
const MESH_PRIMITIVES_OFFSET: u64 = 13 * 8;

pub(crate) struct QueryPool {
    shared: Arc<Shared>,
    raw: VkQueryPool,
    kind: QueryKind,
    /// Pipeline statistics only: the `MESH_PRIMITIVES_GENERATED` counts graphics lists add, and a
    /// compute-only pool for lists of families without graphics (which may not count graphics
    /// statistics).
    mesh: VkQueryPool,
    compute: VkQueryPool,
}

impl QueryPool {
    pub(crate) fn raw(&self) -> VkQueryPool {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        let s = &self.shared;
        for pool in [self.raw, self.mesh, self.compute] {
            if pool != 0 {
                // SAFETY: no pending submission uses the pool (the public API's contract).
                unsafe { (s.fns.vkDestroyQueryPool)(s.raw, pool, ptr::null()) };
            }
        }
    }
}

/// A query pool, reset on the host so its first use needs no reset.
fn raw_pool(
    shared: &Shared,
    ty: VkQueryType,
    count: u32,
    statistics: VkQueryPipelineStatisticFlags,
) -> Result<VkQueryPool> {
    let info = VkQueryPoolCreateInfo {
        queryType: ty,
        queryCount: count,
        pipelineStatistics: statistics,
        ..Default::default()
    };
    let mut raw = 0;
    // SAFETY: a valid create info and out pointer.
    let r = unsafe { (shared.fns.vkCreateQueryPool)(shared.raw, &info, ptr::null(), &mut raw) };
    shared.lost.result(r)?;
    // SAFETY: a fresh pool nothing uses (`hostQueryReset` is Vulkan 1.2 core, on).
    unsafe { (shared.fns.vkResetQueryPool)(shared.raw, raw, 0, count) };
    Ok(raw)
}

/// A query pool of `ty` for internal use (acceleration structure sizes), destroyed with
/// [`destroy_pool`].
pub(crate) fn create_raw_pool(shared: &Shared, ty: VkQueryType, count: u32) -> Result<VkQueryPool> {
    raw_pool(shared, ty, count, 0)
}

pub(crate) fn destroy_pool(shared: &Shared, pool: VkQueryPool) {
    // SAFETY: no pending submission uses the pool (the caller's contract).
    unsafe { (shared.fns.vkDestroyQueryPool)(shared.raw, pool, ptr::null()) };
}

pub(crate) fn create_query_pool(device: &Device, desc: &QueryPoolDesc) -> Result<QueryPool> {
    let s = &device.shared;
    s.lost.check()?;
    let mut pool = QueryPool {
        shared: s.clone(),
        raw: 0,
        kind: desc.kind,
        mesh: 0,
        compute: 0,
    };
    let count = desc.count;
    pool.raw = match desc.kind {
        QueryKind::Timestamp | QueryKind::CopyTimestamp => {
            raw_pool(s, VK_QUERY_TYPE_TIMESTAMP, count, 0)?
        }
        QueryKind::Occlusion | QueryKind::BinaryOcclusion => {
            raw_pool(s, VK_QUERY_TYPE_OCCLUSION, count, 0)?
        }
        QueryKind::PipelineStatistics => {
            raw_pool(s, VK_QUERY_TYPE_PIPELINE_STATISTICS, count, ALL_STATISTICS)?
        }
    };
    // `pool` destroys what was created if a later step fails.
    if desc.kind == QueryKind::PipelineStatistics {
        pool.mesh = raw_pool(s, VK_QUERY_TYPE_MESH_PRIMITIVES_GENERATED_EXT, count, 0)?;
        pool.compute = raw_pool(
            s,
            VK_QUERY_TYPE_PIPELINE_STATISTICS,
            count,
            VK_QUERY_PIPELINE_STATISTIC_COMPUTE_SHADER_INVOCATIONS_BIT,
        )?;
    }
    Ok(pool)
}

/// Whether the list's queue family runs graphics work (and so may count graphics statistics).
fn graphics_family(list: &CommandList) -> bool {
    let f = &list.shared().families;
    f.properties[f.of(list.ty) as usize].queueFlags & VK_QUEUE_GRAPHICS_BIT != 0
}

pub(crate) fn clock_calibration(queue: &Queue) -> Result<ClockCalibration> {
    let s = queue.shared();
    s.lost.check()?;
    // No timestamps on this queue, nothing to calibrate them against.
    queue.timestamp_frequency()?;
    let get = s
        .fns
        .vkGetCalibratedTimestampsKHR
        .filter(|_| s.ext.calibrated_timestamps)
        .ok_or_else(unsupported)?;
    // The clock `marlin_time` reads: QueryPerformanceCounter on Windows, CLOCK_MONOTONIC on
    // Linux.
    let cpu_domain = if cfg!(marlin_platform = "windows") {
        VK_TIME_DOMAIN_QUERY_PERFORMANCE_COUNTER_KHR
    } else {
        VK_TIME_DOMAIN_CLOCK_MONOTONIC_KHR
    };
    if !time_domains(s).contains(&cpu_domain) {
        return Err(unsupported());
    }
    let infos =
        [VK_TIME_DOMAIN_DEVICE_KHR, cpu_domain].map(|domain| VkCalibratedTimestampInfoKHR {
            timeDomain: domain,
            ..Default::default()
        });
    let mut timestamps = [0u64; 2];
    let mut deviation = 0;
    // SAFETY: valid infos and outputs for two timestamps.
    let r = unsafe {
        get(
            s.raw,
            2,
            infos.as_ptr(),
            timestamps.as_mut_ptr(),
            &mut deviation,
        )
    };
    s.lost.result(r)?;
    Ok(ClockCalibration {
        gpu: timestamps[0],
        cpu: timestamps[1],
    })
}

/// The time domains the device calibrates (the device's own is always among them).
fn time_domains(s: &Shared) -> alloc::vec::Vec<VkTimeDomainKHR> {
    let Some(get) = s
        .instance
        .fns
        .vkGetPhysicalDeviceCalibrateableTimeDomainsKHR
    else {
        return alloc::vec::Vec::new();
    };
    let mut count = 0;
    // SAFETY: the two-call idiom with valid pointers.
    unsafe { get(s.physical, &mut count, ptr::null_mut()) };
    let mut domains = alloc::vec![0; count as usize];
    // SAFETY: as above; `domains` holds `count` entries.
    unsafe { get(s.physical, &mut count, domains.as_mut_ptr()) };
    domains.truncate(count as usize);
    domains
}

pub(crate) fn write_timestamp(list: &mut CommandList, pool: &QueryPool, index: u32) {
    // SAFETY: a recording buffer and a query of a timestamp pool (checked by `commands.rs`),
    // reset since it was last written.
    unsafe {
        (list.fns().vkCmdWriteTimestamp2)(
            list.raw(),
            VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            pool.raw,
            index,
        )
    };
}

/// The pools a scoped query of `pool` counts into on `list`, in begin order.
fn scoped_pools(list: &CommandList, pool: &QueryPool) -> alloc::vec::Vec<VkQueryPool> {
    match pool.kind {
        QueryKind::PipelineStatistics if graphics_family(list) => alloc::vec![pool.raw, pool.mesh],
        QueryKind::PipelineStatistics => alloc::vec![pool.compute],
        _ => alloc::vec![pool.raw],
    }
}

pub(crate) fn begin_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
    let flags = match pool.kind {
        QueryKind::Occlusion => VK_QUERY_CONTROL_PRECISE_BIT,
        _ => 0,
    };
    for scoped in scoped_pools(list, pool) {
        // SAFETY: a recording buffer and a reset query of the pool (checked by `commands.rs`);
        // its end follows in the same list.
        unsafe { (list.fns().vkCmdBeginQuery)(list.raw(), scoped, index, flags) };
    }
}

pub(crate) fn end_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
    for scoped in scoped_pools(list, pool).into_iter().rev() {
        // SAFETY: a recording buffer and a query this list began.
        unsafe { (list.fns().vkCmdEndQuery)(list.raw(), scoped, index) };
    }
}

pub(crate) fn resolve_queries(
    list: &mut CommandList,
    pool: &QueryPool,
    first: u32,
    count: u32,
    dst: &crate::Buffer,
    offset: u64,
) {
    assert!(
        list.ty != crate::QueueType::Copy,
        "Vulkan resolves queries on graphics and compute lists"
    );
    let (fns, raw) = (list.fns(), list.raw());
    let dst = dst.raw.raw();
    let stride = pool.kind.result_size();
    let flags = VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT;
    let copy = |from: VkQueryPool, at: u64| {
        // SAFETY: a recording buffer outside a render pass; the range lies in the pool and the
        // records in `dst` (checked by `commands.rs`). Query commands on one queue run in order,
        // so the copy sees the queries' last writes and the reset below follows it.
        unsafe {
            (fns.vkCmdCopyQueryPoolResults)(raw, from, first, count, dst, at, stride, flags);
            (fns.vkCmdResetQueryPool)(raw, from, first, count);
        }
    };
    match pool.kind {
        QueryKind::PipelineStatistics if graphics_family(list) => {
            copy(pool.raw, offset);
            copy(pool.mesh, offset + MESH_PRIMITIVES_OFFSET);
        }
        QueryKind::PipelineStatistics => {
            // Only compute invocations were counted: zero the rest of each record first.
            // SAFETY: a recording buffer; whole words within `dst` (checked by `commands.rs`).
            unsafe { (fns.vkCmdFillBuffer)(raw, dst, offset, stride * count as u64, 0) };
            let barrier = VkMemoryBarrier2 {
                srcStageMask: VK_PIPELINE_STAGE_2_ALL_TRANSFER_BIT,
                srcAccessMask: VK_ACCESS_2_TRANSFER_WRITE_BIT,
                dstStageMask: VK_PIPELINE_STAGE_2_ALL_TRANSFER_BIT,
                dstAccessMask: VK_ACCESS_2_TRANSFER_WRITE_BIT,
                ..Default::default()
            };
            let info = VkDependencyInfo {
                memoryBarrierCount: 1,
                pMemoryBarriers: &barrier,
                ..Default::default()
            };
            // SAFETY: a recording buffer and a valid barrier.
            unsafe { (fns.vkCmdPipelineBarrier2)(raw, &info) };
            copy(pool.compute, offset + COMPUTE_INVOCATIONS_OFFSET);
        }
        _ => copy(pool.raw, offset),
    }
}

/// Conditional rendering reads the low 32 bits of the predicate (resolved query results fit).
/// It covers draws, dispatches and attachment clears; copies run regardless. Without
/// `VK_EXT_conditional_rendering` nothing is skipped: the work runs, which is what an occlusion
/// test that saw something would have chosen anyway. Set inside a render pass, predication must
/// be cleared before the pass ends (Vulkan's conditional rendering cannot cross the boundary).
pub(crate) fn set_predication(
    list: &mut CommandList,
    predicate: Option<(&crate::Buffer, u64, PredicateSkip)>,
) {
    let s = list.shared();
    let (Some(begin), Some(end)) = (
        s.fns.vkCmdBeginConditionalRenderingEXT,
        s.fns.vkCmdEndConditionalRenderingEXT,
    ) else {
        return;
    };
    if !s.ext.conditional_rendering {
        return;
    }
    let raw = list.raw();
    if list.query.predicating {
        // SAFETY: a recording buffer with conditional rendering active.
        unsafe { end(raw) };
        list.query.predicating = false;
    }
    if let Some((buffer, offset, skip)) = predicate {
        let info = VkConditionalRenderingBeginInfoEXT {
            buffer: buffer.raw.raw(),
            offset,
            // Vulkan skips while the value is zero; inverted, while it is not.
            flags: match skip {
                PredicateSkip::IfZero => 0,
                PredicateSkip::IfNonZero => VK_CONDITIONAL_RENDERING_INVERTED_BIT_EXT,
            },
            ..Default::default()
        };
        // SAFETY: a recording buffer without active conditional rendering; an indirect buffer
        // (created with conditional rendering usage) and an aligned offset within it.
        unsafe { begin(raw, &info) };
        list.query.predicating = true;
    }
}

/// Before `vkEndCommandBuffer`: stops predication left on, which Vulkan requires.
pub(crate) fn finish(list: &mut CommandList) {
    if list.query.predicating {
        set_predication(list, None);
    }
}

/// `vkCmdWriteBufferMarker2AMD` where present; otherwise a fill, which lands after earlier
/// transfers rather than at a pipeline stage (enough to see that the GPU got this far).
pub(crate) fn write_marker(
    list: &mut CommandList,
    buffer: &crate::Buffer,
    offset: u64,
    value: u32,
    point: MarkerPoint,
) {
    let s = list.shared();
    let raw = list.raw();
    let buffer = buffer.raw.raw();
    match s
        .fns
        .vkCmdWriteBufferMarker2AMD
        .filter(|_| s.ext.buffer_marker)
    {
        Some(write) => {
            let stage = match point {
                MarkerPoint::Reached => VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                MarkerPoint::Completed => VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            };
            // SAFETY: a recording buffer; an aligned word within a transfer-destination buffer
            // (checked by `commands.rs`).
            unsafe { write(raw, stage, buffer, offset, value) };
        }
        // SAFETY: as above.
        None => unsafe { (s.fns.vkCmdFillBuffer)(raw, buffer, offset, 4, value) },
    }
}
