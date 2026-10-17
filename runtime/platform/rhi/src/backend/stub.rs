//! Placeholder for platforms whose backend is not written yet: [`Instance`] creation fails with
//! `Unsupported`, so no other object can exist.

use core::ffi::c_void;
use core::ptr::NonNull;
use core::time::Duration;

use alloc::vec::Vec;

use crate::contract::{
    Commands, Device, Fences, Handles, Instance, Object, Queue, Queues, Resources, Rhi, Swapchain,
    Types,
};
use crate::{
    AccelerationStructure, AccelerationStructureCopy, AccelerationStructureInputs,
    AccelerationStructureKind, AccelerationStructureSizes, AdapterInfo, Buffer, BufferBarrier,
    BufferDesc, BufferState, BufferTextureCopy, BufferViewDesc, BuildAccelerationStructure,
    BuildFlags, ClearColor, ClockCalibration, ColorSpace, ComputePipelineDesc,
    DepthStencilViewDesc, DeviceDesc, Error, ErrorKind, Features, Fence, FenceWait, Format,
    FormatSupport, FrameStatistics, GraphicsPipelineDesc, HdrMetadata, HeapDesc, IndexFormat,
    IndirectArgs, InstanceDesc, MarkerPoint, MemoryBudget, MemoryRequirements, MeshPipelineDesc,
    OutputInfo, Pageable, PredicateSkip, PresentMode, PresentStatus, QueryPoolDesc, QueueType,
    Rect, RenderPassDesc, RenderTargetView, RenderTargetViewDesc, ResidencyPriority, Result,
    SamplerDesc, ShadingRate, ShadingRateCombiner, SparseBind, SparseTextureLayout, SubmitDesc,
    SubresourceRange, SurfaceHandle, SwapchainDesc, Texture, TextureBarrier, TextureCopy,
    TextureDesc, TextureResolve, TextureViewDesc, Viewport,
};

pub(crate) struct Backend;

pub(crate) enum Never {}

impl Types for Backend {
    // What the future backends will take: Vulkan on Linux, Metal on macOS.
    #[cfg(marlin_platform = "linux")]
    const SHADER_FORMAT: crate::ShaderFormat = crate::ShaderFormat::SpirV;
    #[cfg(marlin_platform = "macos")]
    const SHADER_FORMAT: crate::ShaderFormat = crate::ShaderFormat::MetalLib;
    type Instance = Never;
    type Device = Never;
    type Queue = Never;
    type Fence = Never;
    type CommandPool = Never;
    type CommandList = Never;
    type Swapchain = Never;
    type Buffer = Never;
    type Texture = Never;
    type RenderTargetView = Never;
    type DepthStencilView = Never;
    type ShaderView = Never;
    type Sampler = Never;
    type GraphicsPipeline = Never;
    type ComputePipeline = Never;
    type MeshPipeline = Never;
    type QueryPool = Never;
    type Heap = Never;
    type PipelineCache = Never;
    type AccelerationStructure = Never;
    type SharedHandle = Never;
}

impl Rhi for Backend {}

/// Every object holds a `Never`, so no object can be passed in.
fn never(object: Object<'_, Backend>) -> ! {
    match object {
        Object::Instance(n)
        | Object::Device(n)
        | Object::Queue(n)
        | Object::Fence(n)
        | Object::CommandPool(n)
        | Object::CommandList(n)
        | Object::Swapchain(n)
        | Object::Buffer(n)
        | Object::Texture(n)
        | Object::GraphicsPipeline(n)
        | Object::ComputePipeline(n)
        | Object::MeshPipeline(n)
        | Object::QueryPool(n)
        | Object::Heap(n)
        | Object::AccelerationStructure(n) => match *n {},
    }
}

impl Instance for Backend {
    fn create_instance(_: &InstanceDesc) -> Result<Never> {
        Err(Error::from_kind(ErrorKind::Unsupported))
    }

    fn adapter(instance: &Never, _: u32) -> Option<AdapterInfo> {
        match *instance {}
    }

    fn output(instance: &Never, _: u32, _: u32) -> Option<OutputInfo> {
        match *instance {}
    }

    fn create_device(instance: &Never, _: &DeviceDesc) -> Result<(Never, Queues<Never>)> {
        match *instance {}
    }
}

/// Device functions: the first argument is always a `Never`.
macro_rules! unreachable_device {
    ($($(unsafe)? fn $name:ident($first:ident: $first_ty:ty $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name($first: $first_ty $(, $arg: $ty)*) -> $ret {
            $(let _ = $arg;)*
            match *$first {}
        })*
    };
}

impl Device for Backend {
    unreachable_device! {
        fn adapter_info(device: &Never) -> &AdapterInfo;
        fn features(device: &Never) -> Features;
        fn format_support(device: &Never, format: Format) -> FormatSupport;
        fn status(device: &Never) -> Result<()>;
        fn force_device_lost(device: &Never) -> ();
        fn set_stable_power_state(device: &Never, stable: bool) -> Result<()>;
        fn create_fence(device: &Never, initial: u64, shared: bool) -> Result<Never>;
        fn wait_for_fences(device: &Never, fences: &[(&Fence, u64)], mode: FenceWait, timeout: Option<Duration>) -> Result<bool>;
        fn create_command_pool(device: &Never, ty: QueueType) -> Result<Never>;
        fn create_command_list(device: &Never, pool: &Never) -> Result<Never>;
        fn validation_errors(device: &Never) -> u64;
        fn create_buffer(device: &Never, desc: &BufferDesc) -> Result<Never>;
        fn create_texture(device: &Never, desc: &TextureDesc) -> Result<Never>;
        fn create_render_target_view(device: &Never, texture: &Texture, desc: &RenderTargetViewDesc) -> Result<Never>;
        fn create_depth_stencil_view(device: &Never, texture: &Texture, desc: &DepthStencilViewDesc) -> Result<Never>;
        fn create_texture_view(device: &Never, texture: &Texture, desc: &TextureViewDesc) -> Result<Never>;
        fn create_buffer_view(device: &Never, buffer: &Buffer, desc: &BufferViewDesc) -> Result<Never>;
        fn create_sampler(device: &Never, desc: &SamplerDesc) -> Result<Never>;
        fn create_graphics_pipeline(device: &Never, desc: &GraphicsPipelineDesc<'_>) -> Result<Never>;
        fn create_compute_pipeline(device: &Never, desc: &ComputePipelineDesc<'_>) -> Result<Never>;
        fn create_mesh_pipeline(device: &Never, desc: &MeshPipelineDesc<'_>) -> Result<Never>;
        fn create_pipeline_cache(device: &Never, data: &[u8]) -> Result<Never>;
        fn serialize_pipeline_cache(cache: &Never) -> Result<Vec<u8>>;
        fn create_query_pool(device: &Never, desc: &QueryPoolDesc) -> Result<Never>;
        fn create_heap(device: &Never, desc: &HeapDesc) -> Result<Never>;
        fn buffer_requirements(device: &Never, desc: &BufferDesc) -> MemoryRequirements;
        fn texture_requirements(device: &Never, desc: &TextureDesc) -> MemoryRequirements;
        fn create_placed_buffer(device: &Never, heap: &Never, offset: u64, desc: &BufferDesc) -> Result<Never>;
        fn create_placed_texture(device: &Never, heap: &Never, offset: u64, desc: &TextureDesc) -> Result<Never>;
        fn memory_budget(device: &Never) -> Result<MemoryBudget>;
        fn set_memory_reservation(device: &Never, local: u64, non_local: u64) -> Result<()>;
        fn make_resident(device: &Never, objects: &[Pageable<'_>]) -> Result<()>;
        fn evict(device: &Never, objects: &[Pageable<'_>]) -> Result<()>;
        fn set_residency_priority(device: &Never, objects: &[Pageable<'_>], priority: ResidencyPriority) -> Result<()>;
        fn acceleration_structure_sizes(device: &Never, inputs: &AccelerationStructureInputs<'_>, flags: BuildFlags) -> AccelerationStructureSizes;
        fn create_acceleration_structure(device: &Never, storage: &Never, offset: u64, kind: AccelerationStructureKind) -> Result<(Never, Never)>;
        fn create_sparse_buffer(device: &Never, desc: &BufferDesc) -> Result<Never>;
        fn create_sparse_texture(device: &Never, desc: &TextureDesc) -> Result<Never>;
        fn sparse_texture_layout(device: &Never, texture: &Texture) -> SparseTextureLayout;
    }

    unsafe fn create_swapchain(
        device: &Never,
        _: &Never,
        _: SurfaceHandle,
        _: &SwapchainDesc,
    ) -> Result<Never> {
        match *device {}
    }

    fn export(device: &Never, _: Object<'_, Self>) -> Result<Never> {
        match *device {}
    }

    unsafe fn import_texture(device: &Never, _: &Never, _: &TextureDesc) -> Result<Never> {
        match *device {}
    }

    unsafe fn import_buffer(device: &Never, _: &Never, _: &BufferDesc) -> Result<Never> {
        match *device {}
    }

    unsafe fn import_fence(device: &Never, _: &Never) -> Result<Never> {
        match *device {}
    }

    unsafe fn texture_from_native(
        device: &Never,
        _: *mut c_void,
        _: &TextureDesc,
    ) -> Result<Never> {
        match *device {}
    }

    unsafe fn buffer_from_native(device: &Never, _: *mut c_void, _: &BufferDesc) -> Result<Never> {
        match *device {}
    }
}

impl Handles for Backend {
    fn set_name(object: Object<'_, Self>, _: &str) {
        never(object)
    }

    fn native(object: Object<'_, Self>) -> *mut c_void {
        never(object)
    }

    fn shared_handle_raw(handle: &Never) -> *mut c_void {
        match *handle {}
    }

    unsafe fn shared_handle_from_raw(_: *mut c_void) -> Never {
        panic!("this platform's RHI backend is not written yet: no shared handles")
    }
}

impl Resources for Backend {
    fn buffer_mapped(buffer: &Never) -> Option<NonNull<u8>> {
        match *buffer {}
    }

    fn shader_view_index(view: &Never) -> u32 {
        match *view {}
    }

    fn sampler_index(sampler: &Never) -> u32 {
        match *sampler {}
    }

    fn acceleration_structure_index(structure: &Never) -> u32 {
        match *structure {}
    }

    fn acceleration_structure_address(structure: &Never) -> u64 {
        match *structure {}
    }
}

impl Queue for Backend {
    unsafe fn submit(queue: &Never, _: &SubmitDesc<'_>) -> Result<()> {
        match *queue {}
    }

    fn timestamp_frequency(queue: &Never) -> Result<u64> {
        match *queue {}
    }

    fn clock_calibration(queue: &Never) -> Result<ClockCalibration> {
        match *queue {}
    }

    fn wait_idle(queue: &Never) -> Result<()> {
        match *queue {}
    }

    unsafe fn bind_sparse(queue: &Never, _: &[SparseBind<'_>]) -> Result<()> {
        match *queue {}
    }
}

impl Fences for Backend {
    fn completed_value(fence: &Never) -> u64 {
        match *fence {}
    }

    fn wait(fence: &Never, _: u64, _: Option<Duration>) -> Result<bool> {
        match *fence {}
    }

    fn signal(fence: &Never, _: u64) -> Result<()> {
        match *fence {}
    }
}

/// Recording functions: the list is always a `Never`.
macro_rules! unreachable_commands {
    ($($(unsafe)? fn $name:ident($list:ident: $list_ty:ty $(, $arg:ident: $ty:ty)*);)*) => {
        $(fn $name($list: $list_ty $(, $arg: $ty)*) {
            $(let _ = $arg;)*
            match *$list {}
        })*
    };
}

impl Commands for Backend {
    unsafe fn reset_pool(pool: &mut Never) -> Result<()> {
        match *pool {}
    }

    fn begin(list: &mut Never, _: &mut Never) -> Result<()> {
        match *list {}
    }

    fn end(list: &mut Never) -> Result<()> {
        match *list {}
    }

    unreachable_commands! {
        fn texture_barriers(list: &mut Never, barriers: &[TextureBarrier<'_>]);
        fn buffer_barriers(list: &mut Never, barriers: &[BufferBarrier<'_>]);
        fn memory_barrier(list: &mut Never, before: BufferState, after: BufferState);
        fn begin_render_pass(list: &mut Never, desc: &RenderPassDesc<'_>);
        fn end_render_pass(list: &mut Never);
        fn copy_buffer(list: &mut Never, src: &Buffer, src_offset: u64, dst: &Buffer, dst_offset: u64, size: u64);
        fn copy_buffer_to_texture(list: &mut Never, src: &Buffer, dst: &Texture, region: &BufferTextureCopy);
        fn copy_texture_to_buffer(list: &mut Never, src: &Texture, dst: &Buffer, region: &BufferTextureCopy);
        fn copy_texture(list: &mut Never, src: &Texture, dst: &Texture, region: &TextureCopy);
        fn resolve_texture(list: &mut Never, src: &Texture, dst: &Texture, resolve: &TextureResolve);
        fn fill_buffer(list: &mut Never, buffer: &Buffer, offset: u64, size: u64, value: u32);
        fn clear_storage_texture(list: &mut Never, texture: &Texture, range: &SubresourceRange, value: ClearColor);
        fn write_marker(list: &mut Never, buffer: &Buffer, offset: u64, value: u32, point: MarkerPoint);
        fn set_graphics_pipeline(list: &mut Never, pipeline: &Never);
        fn set_compute_pipeline(list: &mut Never, pipeline: &Never);
        fn set_mesh_pipeline(list: &mut Never, pipeline: &Never);
        fn push_constants(list: &mut Never, offset: u32, data: &[u32]);
        fn set_viewports(list: &mut Never, viewports: &[Viewport]);
        fn set_scissors(list: &mut Never, rects: &[Rect]);
        fn set_stencil_reference(list: &mut Never, reference: u8);
        fn set_blend_constants(list: &mut Never, constants: [f32; 4]);
        fn set_depth_bounds(list: &mut Never, min: f32, max: f32);
        fn set_shading_rate(list: &mut Never, rate: ShadingRate, combiners: [ShadingRateCombiner; 2]);
        fn set_shading_rate_image(list: &mut Never, image: Option<&Texture>);
        fn set_index_buffer(list: &mut Never, buffer: &Buffer, offset: u64, format: IndexFormat);
        fn draw(list: &mut Never, vertex_count: u32, instance_count: u32, first_vertex: u32, first_instance: u32);
        fn draw_indexed(list: &mut Never, index_count: u32, instance_count: u32, first_index: u32, vertex_offset: i32, first_instance: u32);
        fn dispatch(list: &mut Never, x: u32, y: u32, z: u32);
        fn dispatch_mesh(list: &mut Never, x: u32, y: u32, z: u32);
        fn draw_indirect(list: &mut Never, args: &IndirectArgs<'_>);
        fn draw_indexed_indirect(list: &mut Never, args: &IndirectArgs<'_>);
        fn dispatch_indirect(list: &mut Never, args: &IndirectArgs<'_>);
        fn dispatch_mesh_indirect(list: &mut Never, args: &IndirectArgs<'_>);
        fn write_timestamp(list: &mut Never, pool: &Never, index: u32);
        fn begin_query(list: &mut Never, pool: &Never, index: u32);
        fn end_query(list: &mut Never, pool: &Never, index: u32);
        fn resolve_queries(list: &mut Never, pool: &Never, first: u32, count: u32, dst: &Buffer, offset: u64);
        fn set_predication(list: &mut Never, predicate: Option<(&Buffer, u64, PredicateSkip)>);
        fn build_acceleration_structure(list: &mut Never, build: &BuildAccelerationStructure<'_>);
        fn copy_acceleration_structure(list: &mut Never, src: &AccelerationStructure, dst: &AccelerationStructure, mode: AccelerationStructureCopy);
        fn write_compacted_sizes(list: &mut Never, structures: &[&AccelerationStructure], dst: &Buffer, offset: u64);
        fn begin_event(list: &mut Never, name: &str);
        fn end_event(list: &mut Never);
        fn set_marker(list: &mut Never, name: &str);
    }
}

impl Swapchain for Backend {
    fn acquire(swapchain: &mut Never, _: Option<Duration>) -> Result<u32> {
        match *swapchain {}
    }

    fn present(swapchain: &mut Never) -> Result<PresentStatus> {
        match *swapchain {}
    }

    fn resize(swapchain: &mut Never, _: u32, _: u32) -> Result<()> {
        match *swapchain {}
    }

    fn set_present_mode(swapchain: &mut Never, _: PresentMode) {
        match *swapchain {}
    }

    fn supports_color_space(swapchain: &Never, _: ColorSpace) -> bool {
        match *swapchain {}
    }

    fn set_color_space(swapchain: &mut Never, _: ColorSpace) -> Result<()> {
        match *swapchain {}
    }

    fn set_hdr_metadata(swapchain: &mut Never, _: Option<&HdrMetadata>) -> Result<()> {
        match *swapchain {}
    }

    fn containing_output(swapchain: &Never) -> Result<OutputInfo> {
        match *swapchain {}
    }

    fn frame_statistics(swapchain: &Never) -> Result<Option<FrameStatistics>> {
        match *swapchain {}
    }

    fn size(swapchain: &Never) -> (u32, u32) {
        match *swapchain {}
    }

    fn format(swapchain: &Never) -> Format {
        match *swapchain {}
    }

    fn color_space(swapchain: &Never) -> ColorSpace {
        match *swapchain {}
    }

    fn textures(swapchain: &Never) -> &[Texture] {
        match *swapchain {}
    }

    fn views(swapchain: &Never) -> &[RenderTargetView] {
        match *swapchain {}
    }
}
