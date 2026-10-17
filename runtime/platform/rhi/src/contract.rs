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

use core::ffi::c_void;
use core::ptr::NonNull;
use core::time::Duration;

use alloc::vec::Vec;

use crate::{
    AccelerationStructure, AccelerationStructureCopy, AccelerationStructureInputs,
    AccelerationStructureKind, AccelerationStructureSizes, AdapterInfo, Buffer, BufferBarrier,
    BufferDesc, BufferState, BufferTextureCopy, BufferViewDesc, BuildAccelerationStructure,
    BuildFlags, ClearColor, ClockCalibration, ColorSpace, ComputePipelineDesc,
    DepthStencilViewDesc, DeviceDesc, Features, Fence, FenceWait, Format, FormatSupport,
    FrameStatistics, GraphicsPipelineDesc, HdrMetadata, HeapDesc, IndexFormat, IndirectArgs,
    InstanceDesc, MarkerPoint, MemoryBudget, MemoryRequirements, MeshPipelineDesc, OutputInfo,
    Pageable, PredicateSkip, PresentMode, PresentStatus, QueryPoolDesc, QueueType, Rect,
    RenderPassDesc, RenderTargetView, RenderTargetViewDesc, ResidencyPriority, Result, SamplerDesc,
    ShaderFormat, ShadingRate, ShadingRateCombiner, SparseBind, SparseTextureLayout, SubmitDesc,
    SubresourceRange, SurfaceHandle, SwapchainDesc, Texture, TextureBarrier, TextureCopy,
    TextureDesc, TextureResolve, TextureViewDesc, Viewport,
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
    type PipelineCache: Send + Sync;
    type AccelerationStructure: Send + Sync;
    type SharedHandle: Send + Sync;
}

/// A device's queues: graphics always; dedicated compute and copy queues where the API has them.
pub(crate) struct Queues<Q> {
    pub graphics: Q,
    pub compute: Option<Q>,
    pub copy: Option<Q>,
}

/// Any API object, for debug names and native handles.
pub(crate) enum Object<'a, B: Types + ?Sized> {
    Instance(&'a B::Instance),
    Device(&'a B::Device),
    Queue(&'a B::Queue),
    Fence(&'a B::Fence),
    CommandPool(&'a B::CommandPool),
    CommandList(&'a B::CommandList),
    Swapchain(&'a B::Swapchain),
    Buffer(&'a B::Buffer),
    Texture(&'a B::Texture),
    GraphicsPipeline(&'a B::GraphicsPipeline),
    ComputePipeline(&'a B::ComputePipeline),
    MeshPipeline(&'a B::MeshPipeline),
    QueryPool(&'a B::QueryPool),
    Heap(&'a B::Heap),
    AccelerationStructure(&'a B::AccelerationStructure),
}

pub(crate) trait Instance: Types {
    fn create_instance(desc: &InstanceDesc) -> Result<Self::Instance>;
    /// The adapter at `index` in enumeration order (highest performance first), or `None` past
    /// the end.
    fn adapter(instance: &Self::Instance, index: u32) -> Option<AdapterInfo>;
    /// Output (display) `output` of adapter `adapter`, or `None` past the end.
    fn output(instance: &Self::Instance, adapter: u32, output: u32) -> Option<OutputInfo>;
    /// Creates the device and its queues.
    fn create_device(
        instance: &Self::Instance,
        desc: &DeviceDesc,
    ) -> Result<(Self::Device, Queues<Self::Queue>)>;
}

pub(crate) trait Device: Types {
    fn adapter_info(device: &Self::Device) -> &AdapterInfo;
    fn features(device: &Self::Device) -> Features;
    fn format_support(device: &Self::Device, format: Format) -> FormatSupport;
    /// `Err(DeviceLost)` once the device is gone; logs the crash report the first time.
    fn status(device: &Self::Device) -> Result<()>;
    fn force_device_lost(device: &Self::Device);
    fn set_stable_power_state(device: &Self::Device, stable: bool) -> Result<()>;
    fn create_fence(device: &Self::Device, initial: u64, shared: bool) -> Result<Self::Fence>;
    /// `Ok(false)` on timeout.
    fn wait_for_fences(
        device: &Self::Device,
        fences: &[(&Fence, u64)],
        mode: FenceWait,
        timeout: Option<Duration>,
    ) -> Result<bool>;
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
    /// An empty cache if `data` came from another driver or device.
    fn create_pipeline_cache(device: &Self::Device, data: &[u8]) -> Result<Self::PipelineCache>;
    fn serialize_pipeline_cache(cache: &Self::PipelineCache) -> Result<Vec<u8>>;
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

    fn memory_budget(device: &Self::Device) -> Result<MemoryBudget>;
    fn set_memory_reservation(device: &Self::Device, local: u64, non_local: u64) -> Result<()>;
    fn make_resident(device: &Self::Device, objects: &[Pageable<'_>]) -> Result<()>;
    fn evict(device: &Self::Device, objects: &[Pageable<'_>]) -> Result<()>;
    fn set_residency_priority(
        device: &Self::Device,
        objects: &[Pageable<'_>],
        priority: ResidencyPriority,
    ) -> Result<()>;

    fn acceleration_structure_sizes(
        device: &Self::Device,
        inputs: &AccelerationStructureInputs<'_>,
        flags: BuildFlags,
    ) -> AccelerationStructureSizes;
    /// A structure at `offset` of `storage`, a buffer with acceleration structure storage usage,
    /// and a second handle to `storage` sharing its resource (the structure keeps it alive).
    fn create_acceleration_structure(
        device: &Self::Device,
        storage: &Self::Buffer,
        offset: u64,
        kind: AccelerationStructureKind,
    ) -> Result<(Self::Buffer, Self::AccelerationStructure)>;

    fn create_sparse_buffer(device: &Self::Device, desc: &BufferDesc) -> Result<Self::Buffer>;
    fn create_sparse_texture(device: &Self::Device, desc: &TextureDesc) -> Result<Self::Texture>;
    fn sparse_texture_layout(device: &Self::Device, texture: &Texture) -> SparseTextureLayout;

    /// A handle another device or process can open: buffers, textures and fences.
    fn export(device: &Self::Device, object: Object<'_, Self>) -> Result<Self::SharedHandle>;
    /// # Safety
    /// `handle` names a texture that `desc` describes.
    unsafe fn import_texture(
        device: &Self::Device,
        handle: &Self::SharedHandle,
        desc: &TextureDesc,
    ) -> Result<Self::Texture>;
    /// # Safety
    /// `handle` names a buffer that `desc` describes.
    unsafe fn import_buffer(
        device: &Self::Device,
        handle: &Self::SharedHandle,
        desc: &BufferDesc,
    ) -> Result<Self::Buffer>;
    /// # Safety
    /// `handle` names a fence.
    unsafe fn import_fence(
        device: &Self::Device,
        handle: &Self::SharedHandle,
    ) -> Result<Self::Fence>;
    /// # Safety
    /// `raw` is a live native texture of this device that `desc` describes.
    unsafe fn texture_from_native(
        device: &Self::Device,
        raw: *mut c_void,
        desc: &TextureDesc,
    ) -> Result<Self::Texture>;
    /// # Safety
    /// `raw` is a live native buffer of this device that `desc` describes.
    unsafe fn buffer_from_native(
        device: &Self::Device,
        raw: *mut c_void,
        desc: &BufferDesc,
    ) -> Result<Self::Buffer>;
}

pub(crate) trait Handles: Types {
    fn set_name(object: Object<'_, Self>, name: &str);
    /// The API object behind `object` (borrowed: no reference is added).
    fn native(object: Object<'_, Self>) -> *mut c_void;
    fn shared_handle_raw(handle: &Self::SharedHandle) -> *mut c_void;
    /// # Safety
    /// `raw` is an owned handle of the kind `export` returns.
    unsafe fn shared_handle_from_raw(raw: *mut c_void) -> Self::SharedHandle;
}

pub(crate) trait Resources: Types {
    /// The CPU mapping of an upload, readback or GPU upload buffer.
    fn buffer_mapped(buffer: &Self::Buffer) -> Option<NonNull<u8>>;
    /// The view's slot in the bindless resource table.
    fn shader_view_index(view: &Self::ShaderView) -> u32;
    /// The sampler's slot in the bindless sampler table.
    fn sampler_index(sampler: &Self::Sampler) -> u32;
    /// The structure's slot in the bindless resource table.
    fn acceleration_structure_index(structure: &Self::AccelerationStructure) -> u32;
    /// What instance records name the structure by.
    fn acceleration_structure_address(structure: &Self::AccelerationStructure) -> u64;
}

pub(crate) trait Queue: Types {
    /// # Safety
    /// Every resource the lists use stays alive, and every list and its pool stay unreset, until
    /// the GPU finishes them.
    unsafe fn submit(queue: &Self::Queue, desc: &SubmitDesc<'_>) -> Result<()>;
    /// Ticks per second of the queue's timestamps.
    fn timestamp_frequency(queue: &Self::Queue) -> Result<u64>;
    fn clock_calibration(queue: &Self::Queue) -> Result<ClockCalibration>;
    /// Blocks until all submitted work has finished.
    fn wait_idle(queue: &Self::Queue) -> Result<()>;
    /// # Safety
    /// Bound heaps outlive their bindings; `api.rs` validated the ranges.
    unsafe fn bind_sparse(queue: &Self::Queue, binds: &[SparseBind<'_>]) -> Result<()>;
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
    fn memory_barrier(list: &mut Self::CommandList, before: BufferState, after: BufferState);
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
    fn copy_texture(
        list: &mut Self::CommandList,
        src: &Texture,
        dst: &Texture,
        region: &TextureCopy,
    );
    /// `resolve.format` is concrete.
    fn resolve_texture(
        list: &mut Self::CommandList,
        src: &Texture,
        dst: &Texture,
        resolve: &TextureResolve,
    );
    fn fill_buffer(
        list: &mut Self::CommandList,
        buffer: &Buffer,
        offset: u64,
        size: u64,
        value: u32,
    );
    /// `range` is concrete: one mip, real layer counts.
    fn clear_storage_texture(
        list: &mut Self::CommandList,
        texture: &Texture,
        range: &SubresourceRange,
        value: ClearColor,
    );
    fn write_marker(
        list: &mut Self::CommandList,
        buffer: &Buffer,
        offset: u64,
        value: u32,
        point: MarkerPoint,
    );
    fn set_graphics_pipeline(list: &mut Self::CommandList, pipeline: &Self::GraphicsPipeline);
    fn set_compute_pipeline(list: &mut Self::CommandList, pipeline: &Self::ComputePipeline);
    fn set_mesh_pipeline(list: &mut Self::CommandList, pipeline: &Self::MeshPipeline);
    /// Into the push constants of the pipeline kind bound last (graphics before any).
    fn push_constants(list: &mut Self::CommandList, offset: u32, data: &[u32]);
    fn set_viewports(list: &mut Self::CommandList, viewports: &[Viewport]);
    fn set_scissors(list: &mut Self::CommandList, rects: &[Rect]);
    fn set_stencil_reference(list: &mut Self::CommandList, reference: u8);
    fn set_blend_constants(list: &mut Self::CommandList, constants: [f32; 4]);
    fn set_depth_bounds(list: &mut Self::CommandList, min: f32, max: f32);
    fn set_shading_rate(
        list: &mut Self::CommandList,
        rate: ShadingRate,
        combiners: [ShadingRateCombiner; 2],
    );
    fn set_shading_rate_image(list: &mut Self::CommandList, image: Option<&Texture>);
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
    fn dispatch_mesh(list: &mut Self::CommandList, x: u32, y: u32, z: u32);
    fn draw_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn draw_indexed_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn dispatch_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn dispatch_mesh_indirect(list: &mut Self::CommandList, args: &IndirectArgs<'_>);
    fn write_timestamp(list: &mut Self::CommandList, pool: &Self::QueryPool, index: u32);
    fn begin_query(list: &mut Self::CommandList, pool: &Self::QueryPool, index: u32);
    fn end_query(list: &mut Self::CommandList, pool: &Self::QueryPool, index: u32);
    fn resolve_queries(
        list: &mut Self::CommandList,
        pool: &Self::QueryPool,
        first: u32,
        count: u32,
        dst: &Buffer,
        offset: u64,
    );
    fn set_predication(
        list: &mut Self::CommandList,
        predicate: Option<(&Buffer, u64, PredicateSkip)>,
    );
    fn build_acceleration_structure(
        list: &mut Self::CommandList,
        build: &BuildAccelerationStructure<'_>,
    );
    fn copy_acceleration_structure(
        list: &mut Self::CommandList,
        src: &AccelerationStructure,
        dst: &AccelerationStructure,
        mode: AccelerationStructureCopy,
    );
    fn write_compacted_sizes(
        list: &mut Self::CommandList,
        structures: &[&AccelerationStructure],
        dst: &Buffer,
        offset: u64,
    );
    fn begin_event(list: &mut Self::CommandList, name: &str);
    fn end_event(list: &mut Self::CommandList);
    fn set_marker(list: &mut Self::CommandList, name: &str);
}

pub(crate) trait Swapchain: Types {
    /// Waits until the next image may be rendered (frame latency), then returns its index.
    fn acquire(swapchain: &mut Self::Swapchain, timeout: Option<Duration>) -> Result<u32>;
    fn present(swapchain: &mut Self::Swapchain) -> Result<PresentStatus>;
    /// Waits for the GPU to finish with the images, then recreates them.
    fn resize(swapchain: &mut Self::Swapchain, width: u32, height: u32) -> Result<()>;
    fn set_present_mode(swapchain: &mut Self::Swapchain, mode: PresentMode);
    fn supports_color_space(swapchain: &Self::Swapchain, color_space: ColorSpace) -> bool;
    fn set_color_space(swapchain: &mut Self::Swapchain, color_space: ColorSpace) -> Result<()>;
    fn set_hdr_metadata(
        swapchain: &mut Self::Swapchain,
        metadata: Option<&HdrMetadata>,
    ) -> Result<()>;
    fn containing_output(swapchain: &Self::Swapchain) -> Result<OutputInfo>;
    fn frame_statistics(swapchain: &Self::Swapchain) -> Result<Option<FrameStatistics>>;
    fn size(swapchain: &Self::Swapchain) -> (u32, u32);
    fn format(swapchain: &Self::Swapchain) -> Format;
    fn color_space(swapchain: &Self::Swapchain) -> ColorSpace;
    fn textures(swapchain: &Self::Swapchain) -> &[Texture];
    fn views(swapchain: &Self::Swapchain) -> &[RenderTargetView];
}

/// The whole contract.
pub(crate) trait Rhi:
    Instance + Device + Handles + Resources + Queue + Fences + Commands + Swapchain
{
}
