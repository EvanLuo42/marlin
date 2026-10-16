//! The backend contract.
//!
//! A backend is a unit type `Backend` implementing [`Rhi`]: associated types for its objects and
//! static functions on them, the same shape as marlin-pal's contract. The public types in
//! `api.rs` wrap the associated types and forward here, so a backend missing an item or getting a
//! signature wrong fails to compile on its own platform.
//!
//! Backends may name the public wrapper types (`crate::Texture`, ...) where the contract hands
//! them out by reference or needs their descriptions: a swapchain owns its `crate::Texture`s so
//! it can lend `&Texture`.

use core::ptr::NonNull;
use core::time::Duration;

use crate::{
    AdapterInfo, Buffer, BufferBarrier, BufferDesc, BufferTextureCopy, BufferViewDesc,
    ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc, Features, Format, GraphicsPipelineDesc,
    HeapDesc, IndexFormat, IndirectArgs, InstanceDesc, MemoryRequirements, MeshPipelineDesc,
    PresentMode, PresentStatus, QueryPoolDesc, QueueType, Rect, RenderPassDesc, RenderTargetView,
    RenderTargetViewDesc, Result, SamplerDesc, ShaderFormat, SubmitDesc, SurfaceHandle,
    SwapchainDesc, Texture, TextureBarrier, TextureCopy, TextureDesc, TextureViewDesc, Viewport,
};

pub(crate) trait Types {
    const SHADER_FORMAT: ShaderFormat;
    type Instance: Send + Sync;
    type Device: Send + Sync;
    type Queue: Send + Sync;
    type Fence: Send + Sync;
    type CommandPool: Send;
    type CommandList: Send;
    type Swapchain: Send;
    type Buffer: Send + Sync;
    type Texture: Send + Sync;
    type RenderTargetView: Send + Sync;
    type DepthStencilView: Send + Sync;
    type ShaderView: Send + Sync;
    type Sampler: Send + Sync;
    type GraphicsPipeline: Send + Sync;
    type ComputePipeline: Send + Sync;
    type MeshPipeline: Send + Sync;
    type QueryPool: Send + Sync;
    type Heap: Send + Sync;
}

/// A device's queues: graphics always; dedicated compute and copy queues where the API has them.
pub(crate) struct Queues<Q> {
    pub graphics: Q,
    pub compute: Option<Q>,
    pub copy: Option<Q>,
}

pub(crate) trait Instance: Types {
    fn create_instance(desc: &InstanceDesc) -> Result<Self::Instance>;
    /// The adapter at `index` in enumeration order (highest performance first), or `None` past
    /// the end.
    fn adapter(instance: &Self::Instance, index: u32) -> Option<AdapterInfo>;
    /// Creates the device and its queues.
    fn create_device(
        instance: &Self::Instance,
        desc: &DeviceDesc,
    ) -> Result<(Self::Device, Queues<Self::Queue>)>;
}

pub(crate) trait Device: Types {
    fn adapter_info(device: &Self::Device) -> &AdapterInfo;
    fn features(device: &Self::Device) -> Features;
    fn create_fence(device: &Self::Device, initial: u64) -> Result<Self::Fence>;
    fn create_command_pool(device: &Self::Device, ty: QueueType) -> Result<Self::CommandPool>;
    fn create_command_list(
        device: &Self::Device,
        pool: &Self::CommandPool,
    ) -> Result<Self::CommandList>;
    /// # Safety
    /// `surface` must be a live window that outlives the swapchain.
    unsafe fn create_swapchain(
        device: &Self::Device,
        queue: &Self::Queue,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<Self::Swapchain>;
    /// Validation errors logged so far (0 without validation).
    fn validation_errors(device: &Self::Device) -> u64;

    fn create_buffer(device: &Self::Device, desc: &BufferDesc) -> Result<Self::Buffer>;
    fn create_texture(device: &Self::Device, desc: &TextureDesc) -> Result<Self::Texture>;
    fn create_render_target_view(
        device: &Self::Device,
        texture: &Texture,
        desc: &RenderTargetViewDesc,
    ) -> Result<Self::RenderTargetView>;
    fn create_depth_stencil_view(
        device: &Self::Device,
        texture: &Texture,
        desc: &DepthStencilViewDesc,
    ) -> Result<Self::DepthStencilView>;
    fn create_texture_view(
        device: &Self::Device,
        texture: &Texture,
        desc: &TextureViewDesc,
    ) -> Result<Self::ShaderView>;
    fn create_buffer_view(
        device: &Self::Device,
        buffer: &Buffer,
        desc: &BufferViewDesc,
    ) -> Result<Self::ShaderView>;
    fn create_sampler(device: &Self::Device, desc: &SamplerDesc) -> Result<Self::Sampler>;
    fn create_graphics_pipeline(
        device: &Self::Device,
        desc: &GraphicsPipelineDesc<'_>,
    ) -> Result<Self::GraphicsPipeline>;
    fn create_compute_pipeline(
        device: &Self::Device,
        desc: &ComputePipelineDesc<'_>,
    ) -> Result<Self::ComputePipeline>;
    fn create_mesh_pipeline(
        device: &Self::Device,
        desc: &MeshPipelineDesc<'_>,
    ) -> Result<Self::MeshPipeline>;
    fn create_query_pool(device: &Self::Device, desc: &QueryPoolDesc) -> Result<Self::QueryPool>;
    fn create_heap(device: &Self::Device, desc: &HeapDesc) -> Result<Self::Heap>;
    fn buffer_requirements(device: &Self::Device, desc: &BufferDesc) -> MemoryRequirements;
    fn texture_requirements(device: &Self::Device, desc: &TextureDesc) -> MemoryRequirements;
    /// `offset` is aligned and the resource fits (checked by `api.rs`).
    fn create_placed_buffer(
        device: &Self::Device,
        heap: &Self::Heap,
        offset: u64,
        desc: &BufferDesc,
    ) -> Result<Self::Buffer>;
    fn create_placed_texture(
        device: &Self::Device,
        heap: &Self::Heap,
        offset: u64,
        desc: &TextureDesc,
    ) -> Result<Self::Texture>;
}

pub(crate) trait Resources: Types {
    /// The CPU mapping of an upload or readback buffer.
    fn buffer_mapped(buffer: &Self::Buffer) -> Option<NonNull<u8>>;
    /// The view's slot in the bindless resource table.
    fn shader_view_index(view: &Self::ShaderView) -> u32;
    /// The sampler's slot in the bindless sampler table.
    fn sampler_index(sampler: &Self::Sampler) -> u32;
}

pub(crate) trait Queue: Types {
    /// # Safety
    /// Every resource the lists use stays alive, and every list and its pool stay unreset, until
    /// the GPU finishes them.
    unsafe fn submit(queue: &Self::Queue, desc: &SubmitDesc<'_>) -> Result<()>;
    /// Ticks per second of the queue's timestamps.
    fn timestamp_frequency(queue: &Self::Queue) -> Result<u64>;
    /// Blocks until all submitted work has finished.
    fn wait_idle(queue: &Self::Queue) -> Result<()>;
}

pub(crate) trait Fences: Types {
    fn completed_value(fence: &Self::Fence) -> u64;
    /// `Ok(false)` on timeout.
    fn wait(fence: &Self::Fence, value: u64, timeout: Option<Duration>) -> Result<bool>;
    fn signal(fence: &Self::Fence, value: u64) -> Result<()>;
}

/// Recording. Arguments were validated by `api.rs` (ranges, counts, usages).
pub(crate) trait Commands: Types {
    /// # Safety
    /// The GPU has finished every list recorded from the pool.
    unsafe fn reset_pool(pool: &mut Self::CommandPool) -> Result<()>;
    fn begin(list: &mut Self::CommandList, pool: &mut Self::CommandPool) -> Result<()>;
    fn end(list: &mut Self::CommandList) -> Result<()>;
    fn texture_barriers(list: &mut Self::CommandList, barriers: &[TextureBarrier<'_>]);
    fn buffer_barriers(list: &mut Self::CommandList, barriers: &[BufferBarrier<'_>]);
    fn begin_render_pass(list: &mut Self::CommandList, desc: &RenderPassDesc<'_>);
    fn end_render_pass(list: &mut Self::CommandList);
    fn copy_buffer(
        list: &mut Self::CommandList,
        src: &Buffer,
        src_offset: u64,
        dst: &Buffer,
        dst_offset: u64,
        size: u64,
    );
    fn copy_buffer_to_texture(
        list: &mut Self::CommandList,
        src: &Buffer,
        dst: &Texture,
        region: &BufferTextureCopy,
    );
    fn copy_texture_to_buffer(
        list: &mut Self::CommandList,
        src: &Texture,
        dst: &Buffer,
        region: &BufferTextureCopy,
    );
    fn set_graphics_pipeline(list: &mut Self::CommandList, pipeline: &Self::GraphicsPipeline);
    fn set_compute_pipeline(list: &mut Self::CommandList, pipeline: &Self::ComputePipeline);
    /// Into the push constants of the pipeline kind bound last (graphics before any).
    fn push_constants(list: &mut Self::CommandList, offset: u32, data: &[u32]);
    fn set_viewport(list: &mut Self::CommandList, viewport: &Viewport);
    fn set_scissor(list: &mut Self::CommandList, rect: &Rect);
    fn set_stencil_reference(list: &mut Self::CommandList, reference: u8);
    fn set_index_buffer(
        list: &mut Self::CommandList,
        buffer: &Buffer,
        offset: u64,
        format: IndexFormat,
    );
    fn draw(
        list: &mut Self::CommandList,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    );
    fn draw_indexed(
        list: &mut Self::CommandList,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    );
    fn dispatch(list: &mut Self::CommandList, x: u32, y: u32, z: u32);
    fn set_mesh_pipeline(list: &mut Self::CommandList, pipeline: &Self::MeshPipeline);
    fn dispatch_mesh(list: &mut Self::CommandList, x: u32, y: u32, z: u32);
    fn draw_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn draw_indexed_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn dispatch_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn dispatch_mesh_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn write_timestamp(list: &mut Self::CommandList, pool: &Self::QueryPool, index: u32);
    fn resolve_queries(
        list: &mut Self::CommandList,
        pool: &Self::QueryPool,
        first: u32,
        count: u32,
        dst: &Buffer,
        offset: u64,
    );
    fn begin_event(list: &mut Self::CommandList, name: &str);
    fn end_event(list: &mut Self::CommandList);
    fn set_marker(list: &mut Self::CommandList, name: &str);
    fn copy_texture(
        list: &mut Self::CommandList,
        src: &Texture,
        dst: &Texture,
        region: &TextureCopy,
    );
}

pub(crate) trait Swapchain: Types {
    /// Waits until the next image may be rendered (frame latency), then returns its index.
    fn acquire(swapchain: &mut Self::Swapchain, timeout: Option<Duration>) -> Result<u32>;
    fn present(swapchain: &mut Self::Swapchain) -> Result<PresentStatus>;
    /// Waits for the GPU to finish with the images, then recreates them.
    fn resize(swapchain: &mut Self::Swapchain, width: u32, height: u32) -> Result<()>;
    fn set_present_mode(swapchain: &mut Self::Swapchain, mode: PresentMode);
    fn size(swapchain: &Self::Swapchain) -> (u32, u32);
    fn format(swapchain: &Self::Swapchain) -> Format;
    fn textures(swapchain: &Self::Swapchain) -> &[Texture];
    fn views(swapchain: &Self::Swapchain) -> &[RenderTargetView];
}

/// The whole contract.
pub(crate) trait Rhi:
    Instance + Device + Resources + Queue + Fences + Commands + Swapchain
{
}
