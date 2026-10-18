//! Command pools and lists: recording state, the bindless heaps bound at `begin`, barriers
//! (synchronization2, 0016 §5), push constants (push data) and compute dispatch. Copies live in
//! `copy.rs`, render passes and draws in `draw.rs`, queries in `query.rs`.

use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::device::{Device, Shared};
use super::draw::DrawState;
use super::ffi::*;
use super::pipeline::ComputePipeline;
use super::query::QueryState;
use crate::{
    Access, BufferBarrier, BufferState, Layout, MAX_PUSH_CONSTANTS, QueueType, Result, Stage,
    TextureBarrier,
};

pub(crate) struct CommandPool {
    shared: Arc<Shared>,
    raw: VkCommandPool,
    /// The queue its lists run on.
    ty: QueueType,
    /// Command buffers allocated so far; `next` is the first not yet begun since the last reset.
    buffers: Vec<VkCommandBuffer>,
    next: usize,
}

// SAFETY: the pool is used through `&mut` only (one thread at a time), as Vulkan requires.
unsafe impl Send for CommandPool {}

impl CommandPool {
    pub(crate) fn new(device: &Device, ty: QueueType) -> Result<Self> {
        let s = &device.shared;
        let info = VkCommandPoolCreateInfo {
            flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
            queueFamilyIndex: s.families.of(ty),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info and out pointer.
        let r = unsafe { (s.fns.vkCreateCommandPool)(s.raw, &info, ptr::null(), &mut raw) };
        s.lost.result(r)?;
        Ok(Self {
            shared: s.clone(),
            raw,
            ty,
            buffers: Vec::new(),
            next: 0,
        })
    }

    pub(crate) fn raw(&self) -> VkCommandPool {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// # Safety
    /// The GPU has finished every list recorded from the pool.
    pub(crate) unsafe fn reset(&mut self) -> Result<()> {
        let s = &self.shared;
        // SAFETY: forwarded from the caller; the pool is not recording (`&mut self`).
        let r = unsafe { (s.fns.vkResetCommandPool)(s.raw, self.raw, 0) };
        s.lost.result(r)?;
        self.next = 0;
        Ok(())
    }

    /// The next command buffer to record, allocating one when every buffer is in use.
    fn take(&mut self) -> Result<VkCommandBuffer> {
        if self.next == self.buffers.len() {
            let s = &self.shared;
            let info = VkCommandBufferAllocateInfo {
                commandPool: self.raw,
                level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                commandBufferCount: 1,
                ..Default::default()
            };
            let mut buffer = ptr::null_mut();
            // SAFETY: a valid allocate info and out pointer; the pool is ours (`&mut self`).
            let r = unsafe { (s.fns.vkAllocateCommandBuffers)(s.raw, &info, &mut buffer) };
            s.lost.result(r)?;
            self.buffers.push(buffer);
        }
        self.next += 1;
        Ok(self.buffers[self.next - 1])
    }
}

impl Drop for CommandPool {
    fn drop(&mut self) {
        // SAFETY: the GPU is done with the pool's lists (the public API's contract); destroying
        // the pool frees them.
        unsafe { (self.shared.fns.vkDestroyCommandPool)(self.shared.raw, self.raw, ptr::null()) };
    }
}

pub(crate) struct CommandList {
    pub(crate) shared: Arc<Shared>,
    /// The command buffer of the current (or last) recording, from the pool it began with.
    raw: VkCommandBuffer,
    pub(crate) ty: QueueType,
    /// Render pass, pipeline and dynamic state (draw area).
    pub(crate) draw: DrawState,
    /// Open queries and predication (query area).
    pub(crate) query: QueryState,
}

// SAFETY: the list is used through `&mut` only while recording, and its buffer only after `end`.
unsafe impl Send for CommandList {}

impl CommandList {
    pub(crate) fn new(device: &Device, pool: &CommandPool) -> Result<Self> {
        let _ = pool;
        Ok(Self {
            shared: device.shared.clone(),
            raw: ptr::null_mut(),
            ty: QueueType::Graphics,
            draw: DrawState::default(),
            query: QueryState::default(),
        })
    }

    pub(crate) fn raw(&self) -> VkCommandBuffer {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// The device functions, for recording.
    pub(crate) fn fns(&self) -> &DeviceFns {
        &self.shared.fns
    }

    pub(crate) fn begin(&mut self, pool: &mut CommandPool) -> Result<()> {
        let raw = pool.take()?;
        let info = VkCommandBufferBeginInfo {
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            ..Default::default()
        };
        let s = &self.shared;
        // SAFETY: a fresh (or reset) buffer of the pool, which is ours (`&mut`).
        let r = unsafe { (s.fns.vkBeginCommandBuffer)(raw, &info) };
        s.lost.result(r)?;
        self.raw = raw;
        self.ty = pool.ty;
        self.draw.reset();
        self.query = QueryState::default();
        if self.ty != QueueType::Copy {
            let resources = s.bindless.resources.bind_info();
            let samplers = s.bindless.samplers.bind_info();
            let zeros = [0u32; MAX_PUSH_CONSTANTS as usize];
            let push = VkPushDataInfoEXT {
                offset: 0,
                data: VkHostAddressRangeConstEXT {
                    address: zeros.as_ptr().cast(),
                    size: size_of_val(&zeros),
                },
                ..Default::default()
            };
            // SAFETY: a recording buffer of a graphics or compute family; valid heap ranges.
            unsafe {
                (s.fns.vkCmdBindResourceHeapEXT)(raw, &resources);
                (s.fns.vkCmdBindSamplerHeapEXT)(raw, &samplers);
                (s.fns.vkCmdPushDataEXT)(raw, &push);
            }
        }
        Ok(())
    }

    pub(crate) fn end(&mut self) -> Result<()> {
        let s = &self.shared;
        // SAFETY: a recording buffer.
        s.lost
            .result(unsafe { (s.fns.vkEndCommandBuffer)(self.raw) })
    }

    pub(crate) fn texture_barriers(&mut self, barriers: &[TextureBarrier<'_>]) {
        let s = &self.shared;
        let raw: Vec<VkImageMemoryBarrier2> = barriers
            .iter()
            .map(|b| {
                let t = &b.texture.raw;
                let depth = t.is_depth();
                let (src_stage, src_access) = sync(s, b.before.stage, b.before.access);
                let (dst_stage, dst_access) = sync(s, b.after.stage, b.after.access);
                VkImageMemoryBarrier2 {
                    srcStageMask: src_stage,
                    srcAccessMask: src_access,
                    dstStageMask: dst_stage,
                    dstAccessMask: dst_access,
                    oldLayout: layout(s, b.before.layout, depth),
                    newLayout: layout(s, b.after.layout, depth),
                    srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                    dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                    image: t.image(),
                    subresourceRange: VkImageSubresourceRange {
                        aspectMask: t.aspect(),
                        baseMipLevel: b.range.base_mip,
                        levelCount: b.range.mip_count,
                        baseArrayLayer: b.range.base_layer,
                        layerCount: b.range.layer_count,
                    },
                    ..Default::default()
                }
            })
            .collect();
        let info = VkDependencyInfo {
            imageMemoryBarrierCount: raw.len() as u32,
            pImageMemoryBarriers: raw.as_ptr(),
            ..Default::default()
        };
        // SAFETY: a recording buffer and valid barriers.
        unsafe { (s.fns.vkCmdPipelineBarrier2)(self.raw, &info) };
    }

    pub(crate) fn buffer_barriers(&mut self, barriers: &[BufferBarrier<'_>]) {
        let s = &self.shared;
        let raw: Vec<VkBufferMemoryBarrier2> = barriers
            .iter()
            .map(|b| {
                let (src_stage, src_access) = sync(s, b.before.stage, b.before.access);
                let (dst_stage, dst_access) = sync(s, b.after.stage, b.after.access);
                VkBufferMemoryBarrier2 {
                    srcStageMask: src_stage,
                    srcAccessMask: src_access,
                    dstStageMask: dst_stage,
                    dstAccessMask: dst_access,
                    srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                    dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                    buffer: b.buffer.raw.raw(),
                    offset: 0,
                    size: VK_WHOLE_SIZE,
                    ..Default::default()
                }
            })
            .collect();
        let info = VkDependencyInfo {
            bufferMemoryBarrierCount: raw.len() as u32,
            pBufferMemoryBarriers: raw.as_ptr(),
            ..Default::default()
        };
        // SAFETY: a recording buffer and valid barriers.
        unsafe { (s.fns.vkCmdPipelineBarrier2)(self.raw, &info) };
    }

    pub(crate) fn memory_barrier(&mut self, before: BufferState, after: BufferState) {
        let s = &self.shared;
        let (src_stage, src_access) = sync(s, before.stage, before.access);
        let (dst_stage, dst_access) = sync(s, after.stage, after.access);
        let barrier = VkMemoryBarrier2 {
            srcStageMask: src_stage,
            srcAccessMask: src_access,
            dstStageMask: dst_stage,
            dstAccessMask: dst_access,
            ..Default::default()
        };
        let info = VkDependencyInfo {
            memoryBarrierCount: 1,
            pMemoryBarriers: &barrier,
            ..Default::default()
        };
        // SAFETY: a recording buffer and a valid barrier.
        unsafe { (s.fns.vkCmdPipelineBarrier2)(self.raw, &info) };
    }

    pub(crate) fn set_compute_pipeline(&mut self, pipeline: &ComputePipeline) {
        self.draw.compute_pipeline = pipeline.raw();
        // SAFETY: a recording buffer and a live pipeline of this device.
        unsafe {
            (self.shared.fns.vkCmdBindPipeline)(
                self.raw,
                VK_PIPELINE_BIND_POINT_COMPUTE,
                pipeline.raw(),
            )
        };
    }

    /// Push data is shared by every stage and bind point, so this writes what both graphics and
    /// compute pipelines read.
    pub(crate) fn push_constants(&mut self, offset: u32, data: &[u32]) {
        let info = VkPushDataInfoEXT {
            offset: offset * 4,
            data: VkHostAddressRangeConstEXT {
                address: data.as_ptr().cast(),
                size: size_of_val(data),
            },
            ..Default::default()
        };
        // SAFETY: a recording buffer; the range lies within the push data (checked by
        // `commands.rs`).
        unsafe { (self.shared.fns.vkCmdPushDataEXT)(self.raw, &info) };
    }

    pub(crate) fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        // SAFETY: a recording buffer with a compute pipeline bound.
        unsafe { (self.shared.fns.vkCmdDispatch)(self.raw, x, y, z) };
    }
}

// --- Barrier translation (0016 §5) ---

/// `Stage` and `Access` as synchronization2 masks. Stages and accesses of features the device
/// lacks are dropped (they cannot have happened).
pub(crate) fn sync(
    shared: &Shared,
    stage: Stage,
    access: Access,
) -> (VkPipelineStageFlags2, VkAccessFlags2) {
    let ext = &shared.ext;
    let rt = ext.acceleration_structure;
    let mut s: VkPipelineStageFlags2 = 0;
    let has = |bit: Stage| stage.contains(bit) && !bit.is_empty();
    if has(Stage::ALL) || has(Stage::RAYTRACING) {
        s |= VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT;
    }
    if has(Stage::DRAW) {
        s |= VK_PIPELINE_STAGE_2_ALL_GRAPHICS_BIT;
    }
    if has(Stage::INDEX_INPUT) {
        s |= VK_PIPELINE_STAGE_2_INDEX_INPUT_BIT;
    }
    if has(Stage::VERTEX_SHADING) {
        s |= VK_PIPELINE_STAGE_2_PRE_RASTERIZATION_SHADERS_BIT;
    }
    if has(Stage::PIXEL_SHADING) {
        s |= VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT;
    }
    if has(Stage::DEPTH_STENCIL) {
        s |= VK_PIPELINE_STAGE_2_EARLY_FRAGMENT_TESTS_BIT
            | VK_PIPELINE_STAGE_2_LATE_FRAGMENT_TESTS_BIT;
    }
    if has(Stage::RENDER_TARGET) {
        s |= VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT;
    }
    if has(Stage::COMPUTE_SHADING) {
        s |= VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT;
    }
    if has(Stage::COPY) {
        s |= VK_PIPELINE_STAGE_2_COPY_BIT;
    }
    if has(Stage::RESOLVE) {
        s |= VK_PIPELINE_STAGE_2_RESOLVE_BIT;
    }
    if has(Stage::EXECUTE_INDIRECT) {
        s |= VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT | VK_PIPELINE_STAGE_2_COMMAND_PREPROCESS_BIT_EXT;
        if ext.conditional_rendering {
            s |= VK_PIPELINE_STAGE_2_CONDITIONAL_RENDERING_BIT_EXT;
        }
    }
    if has(Stage::ALL_SHADING) {
        s |= VK_PIPELINE_STAGE_2_PRE_RASTERIZATION_SHADERS_BIT
            | VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT
            | VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT;
    }
    if has(Stage::NON_PIXEL_SHADING) {
        s |= VK_PIPELINE_STAGE_2_PRE_RASTERIZATION_SHADERS_BIT
            | VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT;
    }
    if has(Stage::CLEAR_STORAGE) {
        s |= VK_PIPELINE_STAGE_2_CLEAR_BIT;
    }
    let build = has(Stage::BUILD_ACCELERATION_STRUCTURE)
        || has(Stage::COPY_ACCELERATION_STRUCTURE)
        || has(Stage::EMIT_ACCELERATION_STRUCTURE_INFO);
    if build && rt {
        s |= VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR;
    }
    if access.contains(Access::SHADING_RATE_SOURCE) && ext.fragment_shading_rate {
        s |= VK_PIPELINE_STAGE_2_FRAGMENT_SHADING_RATE_ATTACHMENT_BIT_KHR;
    }

    let mut a: VkAccessFlags2 = 0;
    let acc = |bit: Access| access.contains(bit) && !bit.is_empty();
    if acc(Access::VERTEX_BUFFER) {
        a |= VK_ACCESS_2_VERTEX_ATTRIBUTE_READ_BIT;
    }
    if acc(Access::CONSTANT_BUFFER) {
        a |= VK_ACCESS_2_UNIFORM_READ_BIT;
    }
    if acc(Access::INDEX_BUFFER) {
        a |= VK_ACCESS_2_INDEX_READ_BIT;
    }
    if acc(Access::RENDER_TARGET) {
        a |= VK_ACCESS_2_COLOR_ATTACHMENT_READ_BIT | VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT;
    }
    if acc(Access::UNORDERED_ACCESS) {
        if has(Stage::CLEAR_STORAGE) {
            a |= VK_ACCESS_2_TRANSFER_WRITE_BIT;
        }
        if build && rt {
            a |= VK_ACCESS_2_ACCELERATION_STRUCTURE_READ_BIT_KHR
                | VK_ACCESS_2_ACCELERATION_STRUCTURE_WRITE_BIT_KHR;
        }
        if s & !(VK_PIPELINE_STAGE_2_CLEAR_BIT
            | VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR)
            != 0
        {
            a |= VK_ACCESS_2_SHADER_STORAGE_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT;
        }
    }
    if acc(Access::DEPTH_STENCIL_WRITE) {
        a |= VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT
            | VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
    }
    if acc(Access::DEPTH_STENCIL_READ) {
        a |= VK_ACCESS_2_DEPTH_STENCIL_ATTACHMENT_READ_BIT;
    }
    if acc(Access::SHADER_RESOURCE) {
        a |= VK_ACCESS_2_SHADER_READ_BIT;
    }
    if acc(Access::INDIRECT_ARGUMENT) {
        a |= VK_ACCESS_2_INDIRECT_COMMAND_READ_BIT;
        if ext.conditional_rendering {
            a |= VK_ACCESS_2_CONDITIONAL_RENDERING_READ_BIT_EXT;
        }
    }
    if acc(Access::COPY_DST) || acc(Access::RESOLVE_DST) {
        a |= VK_ACCESS_2_TRANSFER_WRITE_BIT;
    }
    if acc(Access::COPY_SRC) || acc(Access::RESOLVE_SRC) {
        a |= VK_ACCESS_2_TRANSFER_READ_BIT;
    }
    if rt && acc(Access::ACCELERATION_STRUCTURE_READ) {
        a |= VK_ACCESS_2_ACCELERATION_STRUCTURE_READ_BIT_KHR;
    }
    if rt && acc(Access::ACCELERATION_STRUCTURE_WRITE) {
        a |= VK_ACCESS_2_ACCELERATION_STRUCTURE_WRITE_BIT_KHR;
    }
    if acc(Access::SHADING_RATE_SOURCE) && ext.fragment_shading_rate {
        a |= VK_ACCESS_2_FRAGMENT_SHADING_RATE_ATTACHMENT_READ_BIT_KHR;
    }
    (s, a)
}

/// `Layout` as a Vulkan image layout. Depth formats read by shaders stay in the depth read-only
/// layout, so one descriptor layout serves both depth test reads and sampling.
pub(crate) fn layout(shared: &Shared, layout: Layout, depth: bool) -> VkImageLayout {
    match layout {
        Layout::Undefined => VK_IMAGE_LAYOUT_UNDEFINED,
        Layout::Present if shared.ext.swapchain => VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
        Layout::Present | Layout::Common | Layout::UnorderedAccess => VK_IMAGE_LAYOUT_GENERAL,
        Layout::RenderTarget => VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        Layout::DepthStencilWrite => VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        Layout::DepthStencilRead => VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL,
        Layout::ShaderResource if depth => VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL,
        Layout::ShaderResource => VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        Layout::CopySrc | Layout::ResolveSrc => VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        Layout::CopyDst | Layout::ResolveDst => VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        Layout::ShadingRateSource => VK_IMAGE_LAYOUT_FRAGMENT_SHADING_RATE_ATTACHMENT_OPTIMAL_KHR,
    }
}
