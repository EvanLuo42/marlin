//! Vulkan backend (Windows, Linux). Design: docs/design/0016-rhi-vulkan.md.
//!
//! The loader (`vulkan-1.dll`, `libvulkan.so.1`) is opened at run time, so a machine without
//! Vulkan gets `Unsupported` instead of a loader error. Baseline: Vulkan 1.4 with
//! `VK_EXT_descriptor_heap`, `VK_EXT_mesh_shader` and `VK_EXT_device_generated_commands`.
//! Bindless tables are descriptor heaps; there are no descriptor sets or pipeline layouts.
//!
//! The contract impls below only forward: each function lives in the file of its area (0016 §2).

// TEMPORARY: helpers of areas still being written (0016 §10) are unused until they land.
#![allow(dead_code)]

mod command;
mod copy;
mod descriptor;
mod device;
mod diag;
mod draw;
mod ffi;
mod instance;
mod memory;
mod pipeline;
mod query;
mod queue;
mod raytracing;
mod residency;
mod resource;
mod share;
mod swapchain;

use core::ffi::c_void;
use core::ptr::NonNull;
use core::time::Duration;

use alloc::vec::Vec;

use crate::contract::{Object, Queues, Rhi, Types};
use crate::{
    AccelerationStructureCopy, AccelerationStructureInputs, AccelerationStructureKind,
    AccelerationStructureSizes, AdapterInfo, BufferBarrier, BufferDesc, BufferState,
    BufferTextureCopy, BufferViewDesc, BuildAccelerationStructure, BuildFlags, ClearColor,
    ClockCalibration, ColorSpace, ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc, Error,
    ErrorKind, Features, Fence as PublicFence, FenceWait, Format, FormatSupport, FrameStatistics,
    GraphicsPipelineDesc, HdrMetadata, HeapDesc, IndexFormat, IndirectArgs, InstanceDesc,
    MarkerPoint, MemoryBudget, MemoryRequirements, MeshPipelineDesc, OutputInfo, Pageable,
    PredicateSkip, PresentMode, PresentStatus, QueryPoolDesc, QueueType, Rect, RenderPassDesc,
    RenderTargetViewDesc, ResidencyPriority, Result, SamplerDesc, ShadingRate, ShadingRateCombiner,
    SparseBind, SparseTextureLayout, SubmitDesc, SubresourceRange, SurfaceHandle, SwapchainDesc,
    TextureBarrier, TextureCopy, TextureDesc, TextureResolve, TextureViewDesc, Viewport,
};
use ffi::*;

pub(crate) struct Backend;

impl Types for Backend {
    const SHADER_FORMAT: crate::ShaderFormat = crate::ShaderFormat::SpirV;
    const GRAPHICS_API: crate::GraphicsApi = crate::GraphicsApi::Vulkan;
    type Instance = instance::Instance;
    type Device = device::Device;
    type Queue = queue::Queue;
    type Fence = queue::Fence;
    type CommandPool = command::CommandPool;
    type CommandList = command::CommandList;
    type Swapchain = swapchain::Swapchain;
    type Buffer = resource::Buffer;
    type Texture = resource::Texture;
    type RenderTargetView = resource::RenderTargetView;
    type DepthStencilView = resource::DepthStencilView;
    type ShaderView = descriptor::Descriptor;
    type Sampler = descriptor::Descriptor;
    type GraphicsPipeline = pipeline::GraphicsPipeline;
    type ComputePipeline = pipeline::ComputePipeline;
    type MeshPipeline = pipeline::MeshPipeline;
    type QueryPool = query::QueryPool;
    type Heap = memory::Heap;
    type PipelineCache = pipeline::PipelineCache;
    type AccelerationStructure = raytracing::AccelerationStructure;
    type SharedHandle = share::SharedHandle;
}

impl Rhi for Backend {}

// --- Errors ---

/// Maps a failed `VkResult` to an [`Error`].
fn error(result: VkResult) -> Error {
    let kind = match result {
        VK_ERROR_OUT_OF_HOST_MEMORY
        | VK_ERROR_OUT_OF_DEVICE_MEMORY
        | VK_ERROR_OUT_OF_POOL_MEMORY
        | VK_ERROR_TOO_MANY_OBJECTS => ErrorKind::OutOfMemory,
        VK_ERROR_DEVICE_LOST => ErrorKind::DeviceLost,
        VK_ERROR_INCOMPATIBLE_DRIVER
        | VK_ERROR_INITIALIZATION_FAILED
        | VK_ERROR_EXTENSION_NOT_PRESENT
        | VK_ERROR_FEATURE_NOT_PRESENT
        | VK_ERROR_LAYER_NOT_PRESENT
        | VK_ERROR_FORMAT_NOT_SUPPORTED => ErrorKind::Unsupported,
        VK_ERROR_INVALID_EXTERNAL_HANDLE => ErrorKind::InvalidInput,
        VK_TIMEOUT => ErrorKind::TimedOut,
        _ => ErrorKind::Other,
    };
    Error::new(kind, result)
}

/// `Err` for the error codes (negative); success codes (`VK_SUBOPTIMAL_KHR`, ...) pass.
fn check(result: VkResult) -> Result<()> {
    if result < 0 {
        Err(error(result))
    } else {
        Ok(())
    }
}

fn unsupported() -> Error {
    Error::from_kind(ErrorKind::Unsupported)
}

fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

/// A non-dispatchable handle as the pointer `as_raw` hands out.
fn handle_ptr(handle: u64) -> *mut c_void {
    handle as usize as *mut c_void
}

// --- Contract ---

impl crate::contract::Instance for Backend {
    fn create_instance(desc: &InstanceDesc) -> Result<instance::Instance> {
        instance::Instance::new(desc)
    }

    fn adapter(instance: &instance::Instance, index: u32) -> Option<AdapterInfo> {
        instance.adapter(index)
    }

    fn output(instance: &instance::Instance, adapter: u32, output: u32) -> Option<OutputInfo> {
        swapchain::output(instance, adapter, output)
    }

    fn create_device(
        instance: &instance::Instance,
        desc: &DeviceDesc,
    ) -> Result<(device::Device, Queues<queue::Queue>)> {
        device::create(instance, desc)
    }
}

impl crate::contract::Device for Backend {
    fn adapter_info(device: &device::Device) -> &AdapterInfo {
        &device.info
    }

    fn features(device: &device::Device) -> Features {
        device.features
    }

    fn format_support(device: &device::Device, format: Format) -> FormatSupport {
        resource::format_support(device, format)
    }

    fn status(device: &device::Device) -> Result<()> {
        device.lost.check()
    }

    fn force_device_lost(device: &device::Device) {
        device.lost.force();
    }

    fn set_stable_power_state(_: &device::Device, _: bool) -> Result<()> {
        // Vulkan has no portable way to pin the clocks.
        Err(unsupported())
    }

    fn create_fence(device: &device::Device, initial: u64, shared: bool) -> Result<queue::Fence> {
        queue::Fence::new(device, initial, shared)
    }

    fn wait_for_fences(
        device: &device::Device,
        fences: &[(&PublicFence, u64)],
        mode: FenceWait,
        timeout: Option<Duration>,
    ) -> Result<bool> {
        queue::wait_for_fences(device, fences, mode, timeout)
    }

    fn create_command_pool(device: &device::Device, ty: QueueType) -> Result<command::CommandPool> {
        command::CommandPool::new(device, ty)
    }

    fn create_command_list(
        device: &device::Device,
        pool: &command::CommandPool,
    ) -> Result<command::CommandList> {
        command::CommandList::new(device, pool)
    }

    unsafe fn create_swapchain(
        device: &device::Device,
        queue: &queue::Queue,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<swapchain::Swapchain> {
        // SAFETY: forwarded from the caller.
        unsafe { swapchain::Swapchain::new(device, queue, surface, desc) }
    }

    fn validation_errors(device: &device::Device) -> u64 {
        device.instance.validation_errors()
    }

    fn create_buffer(device: &device::Device, desc: &BufferDesc) -> Result<resource::Buffer> {
        resource::create_buffer(device, desc)
    }

    fn create_texture(device: &device::Device, desc: &TextureDesc) -> Result<resource::Texture> {
        resource::create_texture(device, desc)
    }

    fn create_render_target_view(
        device: &device::Device,
        texture: &crate::Texture,
        desc: &RenderTargetViewDesc,
    ) -> Result<resource::RenderTargetView> {
        resource::create_render_target_view(device, texture, desc)
    }

    fn create_depth_stencil_view(
        device: &device::Device,
        texture: &crate::Texture,
        desc: &DepthStencilViewDesc,
    ) -> Result<resource::DepthStencilView> {
        resource::create_depth_stencil_view(device, texture, desc)
    }

    fn create_texture_view(
        device: &device::Device,
        texture: &crate::Texture,
        desc: &TextureViewDesc,
    ) -> Result<descriptor::Descriptor> {
        resource::create_texture_view(device, texture, desc)
    }

    fn create_buffer_view(
        device: &device::Device,
        buffer: &crate::Buffer,
        desc: &BufferViewDesc,
    ) -> Result<descriptor::Descriptor> {
        resource::create_buffer_view(device, buffer, desc)
    }

    fn create_sampler(
        device: &device::Device,
        desc: &SamplerDesc,
    ) -> Result<descriptor::Descriptor> {
        resource::create_sampler(device, desc)
    }

    fn create_graphics_pipeline(
        device: &device::Device,
        desc: &GraphicsPipelineDesc<'_>,
    ) -> Result<pipeline::GraphicsPipeline> {
        pipeline::create_graphics_pipeline(device, desc)
    }

    fn create_compute_pipeline(
        device: &device::Device,
        desc: &ComputePipelineDesc<'_>,
    ) -> Result<pipeline::ComputePipeline> {
        pipeline::create_compute_pipeline(device, desc)
    }

    fn create_mesh_pipeline(
        device: &device::Device,
        desc: &MeshPipelineDesc<'_>,
    ) -> Result<pipeline::MeshPipeline> {
        pipeline::create_mesh_pipeline(device, desc)
    }

    fn create_pipeline_cache(
        device: &device::Device,
        data: &[u8],
    ) -> Result<pipeline::PipelineCache> {
        pipeline::PipelineCache::new(device, data)
    }

    fn serialize_pipeline_cache(cache: &pipeline::PipelineCache) -> Result<Vec<u8>> {
        cache.serialize()
    }

    fn create_query_pool(
        device: &device::Device,
        desc: &QueryPoolDesc,
    ) -> Result<query::QueryPool> {
        query::create_query_pool(device, desc)
    }

    fn create_heap(device: &device::Device, desc: &HeapDesc) -> Result<memory::Heap> {
        memory::Heap::new(device, desc)
    }

    fn buffer_requirements(device: &device::Device, desc: &BufferDesc) -> MemoryRequirements {
        resource::buffer_requirements(device, desc)
    }

    fn texture_requirements(device: &device::Device, desc: &TextureDesc) -> MemoryRequirements {
        resource::texture_requirements(device, desc)
    }

    fn create_placed_buffer(
        device: &device::Device,
        heap: &memory::Heap,
        offset: u64,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        resource::create_placed_buffer(device, heap, offset, desc)
    }

    fn create_placed_texture(
        device: &device::Device,
        heap: &memory::Heap,
        offset: u64,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        resource::create_placed_texture(device, heap, offset, desc)
    }

    fn memory_budget(device: &device::Device) -> Result<MemoryBudget> {
        residency::memory_budget(device)
    }

    fn set_memory_reservation(device: &device::Device, local: u64, non_local: u64) -> Result<()> {
        residency::set_memory_reservation(device, local, non_local)
    }

    fn make_resident(device: &device::Device, objects: &[Pageable<'_>]) -> Result<()> {
        residency::make_resident(device, objects)
    }

    fn evict(device: &device::Device, objects: &[Pageable<'_>]) -> Result<()> {
        residency::evict(device, objects)
    }

    fn set_residency_priority(
        device: &device::Device,
        objects: &[Pageable<'_>],
        priority: ResidencyPriority,
    ) -> Result<()> {
        residency::set_residency_priority(device, objects, priority)
    }

    fn acceleration_structure_sizes(
        device: &device::Device,
        inputs: &AccelerationStructureInputs<'_>,
        flags: BuildFlags,
    ) -> AccelerationStructureSizes {
        raytracing::sizes(device, inputs, flags)
    }

    fn create_acceleration_structure(
        device: &device::Device,
        storage: &resource::Buffer,
        offset: u64,
        size: u64,
        kind: AccelerationStructureKind,
    ) -> Result<(resource::Buffer, raytracing::AccelerationStructure)> {
        raytracing::create(device, storage, offset, size, kind)
    }

    fn create_sparse_buffer(
        device: &device::Device,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        resource::create_sparse_buffer(device, desc)
    }

    fn create_sparse_texture(
        device: &device::Device,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        resource::create_sparse_texture(device, desc)
    }

    fn export(device: &device::Device, object: Object<'_, Self>) -> Result<share::SharedHandle> {
        share::export(device, object)
    }

    unsafe fn import_texture(
        device: &device::Device,
        handle: &share::SharedHandle,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        // SAFETY: forwarded from the caller.
        unsafe { share::import_texture(device, handle, desc) }
    }

    unsafe fn import_buffer(
        device: &device::Device,
        handle: &share::SharedHandle,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        // SAFETY: forwarded from the caller.
        unsafe { share::import_buffer(device, handle, desc) }
    }

    unsafe fn import_fence(
        device: &device::Device,
        handle: &share::SharedHandle,
    ) -> Result<queue::Fence> {
        // SAFETY: forwarded from the caller.
        unsafe { share::import_fence(device, handle) }
    }

    unsafe fn texture_from_native(
        device: &device::Device,
        raw: *mut c_void,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        // SAFETY: forwarded from the caller.
        unsafe { share::texture_from_native(device, raw, desc) }
    }

    unsafe fn buffer_from_native(
        device: &device::Device,
        raw: *mut c_void,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        // SAFETY: forwarded from the caller.
        unsafe { share::buffer_from_native(device, raw, desc) }
    }
}

impl crate::contract::Handles for Backend {
    fn set_name(object: Object<'_, Self>, name: &str) {
        diag::set_name(object, name);
    }

    fn native(object: Object<'_, Self>) -> *mut c_void {
        diag::native(object)
    }

    fn shared_handle_raw(handle: &share::SharedHandle) -> *mut c_void {
        handle.as_raw()
    }

    unsafe fn shared_handle_from_raw(raw: *mut c_void) -> share::SharedHandle {
        // SAFETY: forwarded from the caller.
        unsafe { share::SharedHandle::from_raw(raw) }
    }
}

impl crate::contract::Resources for Backend {
    fn buffer_mapped(buffer: &resource::Buffer) -> Option<NonNull<u8>> {
        buffer.mapped()
    }

    fn shader_view_index(view: &descriptor::Descriptor) -> u32 {
        view.index()
    }

    fn sampler_index(sampler: &descriptor::Descriptor) -> u32 {
        sampler.index()
    }

    fn acceleration_structure_index(structure: &raytracing::AccelerationStructure) -> u32 {
        structure.index()
    }

    fn acceleration_structure_address(structure: &raytracing::AccelerationStructure) -> u64 {
        structure.address()
    }

    fn sparse_texture_layout(texture: &resource::Texture) -> SparseTextureLayout {
        resource::sparse_texture_layout(texture)
    }
}

impl crate::contract::Queue for Backend {
    unsafe fn submit(queue: &queue::Queue, desc: &SubmitDesc<'_>) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { queue.submit(desc) }
    }

    fn timestamp_frequency(queue: &queue::Queue) -> Result<u64> {
        queue.timestamp_frequency()
    }

    fn clock_calibration(queue: &queue::Queue) -> Result<ClockCalibration> {
        query::clock_calibration(queue)
    }

    fn wait_idle(queue: &queue::Queue) -> Result<()> {
        queue.wait_idle()
    }

    unsafe fn bind_sparse(queue: &queue::Queue, binds: &[SparseBind<'_>]) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { resource::bind_sparse(queue, binds) }
    }
}

impl crate::contract::Fences for Backend {
    fn completed_value(fence: &queue::Fence) -> u64 {
        fence.completed_value()
    }

    fn wait(fence: &queue::Fence, value: u64, timeout: Option<Duration>) -> Result<bool> {
        fence.wait(value, timeout)
    }

    fn signal(fence: &queue::Fence, value: u64) -> Result<()> {
        fence.signal(value)
    }
}

impl crate::contract::Commands for Backend {
    unsafe fn reset_pool(pool: &mut command::CommandPool) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { pool.reset() }
    }

    fn begin(list: &mut command::CommandList, pool: &mut command::CommandPool) -> Result<()> {
        list.begin(pool)
    }

    fn end(list: &mut command::CommandList) -> Result<()> {
        list.end()
    }

    fn texture_barriers(list: &mut command::CommandList, barriers: &[TextureBarrier<'_>]) {
        list.texture_barriers(barriers);
    }

    fn buffer_barriers(list: &mut command::CommandList, barriers: &[BufferBarrier<'_>]) {
        list.buffer_barriers(barriers);
    }

    fn memory_barrier(list: &mut command::CommandList, before: BufferState, after: BufferState) {
        list.memory_barrier(before, after);
    }

    fn begin_render_pass(list: &mut command::CommandList, desc: &RenderPassDesc<'_>) {
        draw::begin_render_pass(list, desc);
    }

    fn end_render_pass(list: &mut command::CommandList) {
        draw::end_render_pass(list);
    }

    fn copy_buffer(
        list: &mut command::CommandList,
        src: &crate::Buffer,
        src_offset: u64,
        dst: &crate::Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        copy::copy_buffer(list, src, src_offset, dst, dst_offset, size);
    }

    fn copy_buffer_to_texture(
        list: &mut command::CommandList,
        src: &crate::Buffer,
        dst: &crate::Texture,
        region: &BufferTextureCopy,
    ) {
        copy::copy_buffer_to_texture(list, src, dst, region);
    }

    fn copy_texture_to_buffer(
        list: &mut command::CommandList,
        src: &crate::Texture,
        dst: &crate::Buffer,
        region: &BufferTextureCopy,
    ) {
        copy::copy_texture_to_buffer(list, src, dst, region);
    }

    fn copy_texture(
        list: &mut command::CommandList,
        src: &crate::Texture,
        dst: &crate::Texture,
        region: &TextureCopy,
    ) {
        copy::copy_texture(list, src, dst, region);
    }

    fn resolve_texture(
        list: &mut command::CommandList,
        src: &crate::Texture,
        dst: &crate::Texture,
        resolve: &TextureResolve,
    ) {
        copy::resolve_texture(list, src, dst, resolve);
    }

    fn fill_buffer(
        list: &mut command::CommandList,
        buffer: &crate::Buffer,
        offset: u64,
        size: u64,
        value: u32,
    ) {
        copy::fill_buffer(list, buffer, offset, size, value);
    }

    fn clear_storage_texture(
        list: &mut command::CommandList,
        texture: &crate::Texture,
        range: &SubresourceRange,
        value: ClearColor,
    ) {
        copy::clear_storage_texture(list, texture, range, value);
    }

    fn write_marker(
        list: &mut command::CommandList,
        buffer: &crate::Buffer,
        offset: u64,
        value: u32,
        point: MarkerPoint,
    ) {
        query::write_marker(list, buffer, offset, value, point);
    }

    fn set_graphics_pipeline(
        list: &mut command::CommandList,
        pipeline: &pipeline::GraphicsPipeline,
    ) {
        draw::set_graphics_pipeline(list, pipeline);
    }

    fn set_compute_pipeline(list: &mut command::CommandList, pipeline: &pipeline::ComputePipeline) {
        list.set_compute_pipeline(pipeline);
    }

    fn set_mesh_pipeline(list: &mut command::CommandList, pipeline: &pipeline::MeshPipeline) {
        draw::set_mesh_pipeline(list, pipeline);
    }

    fn push_constants(list: &mut command::CommandList, offset: u32, data: &[u32]) {
        list.push_constants(offset, data);
    }

    fn set_viewports(list: &mut command::CommandList, viewports: &[Viewport]) {
        draw::set_viewports(list, viewports);
    }

    fn set_scissors(list: &mut command::CommandList, rects: &[Rect]) {
        draw::set_scissors(list, rects);
    }

    fn set_stencil_reference(list: &mut command::CommandList, reference: u8) {
        draw::set_stencil_reference(list, reference);
    }

    fn set_blend_constants(list: &mut command::CommandList, constants: [f32; 4]) {
        draw::set_blend_constants(list, constants);
    }

    fn set_depth_bounds(list: &mut command::CommandList, min: f32, max: f32) {
        draw::set_depth_bounds(list, min, max);
    }

    fn set_shading_rate(
        list: &mut command::CommandList,
        rate: ShadingRate,
        combiners: [ShadingRateCombiner; 2],
    ) {
        draw::set_shading_rate(list, rate, combiners);
    }

    fn set_shading_rate_image(list: &mut command::CommandList, image: Option<&crate::Texture>) {
        draw::set_shading_rate_image(list, image);
    }

    fn set_index_buffer(
        list: &mut command::CommandList,
        buffer: &crate::Buffer,
        offset: u64,
        format: IndexFormat,
    ) {
        draw::set_index_buffer(list, buffer, offset, format);
    }

    fn draw(
        list: &mut command::CommandList,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        draw::draw(
            list,
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        );
    }

    fn draw_indexed(
        list: &mut command::CommandList,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        draw::draw_indexed(
            list,
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        );
    }

    fn dispatch(list: &mut command::CommandList, x: u32, y: u32, z: u32) {
        list.dispatch(x, y, z);
    }

    fn dispatch_mesh(list: &mut command::CommandList, x: u32, y: u32, z: u32) {
        draw::dispatch_mesh(list, x, y, z);
    }

    fn draw_indirect(list: &mut command::CommandList, args: &IndirectArgs<'_>) {
        draw::draw_indirect(list, args);
    }

    fn draw_indexed_indirect(list: &mut command::CommandList, args: &IndirectArgs<'_>) {
        draw::draw_indexed_indirect(list, args);
    }

    fn dispatch_indirect(list: &mut command::CommandList, args: &IndirectArgs<'_>) {
        draw::dispatch_indirect(list, args);
    }

    fn dispatch_mesh_indirect(list: &mut command::CommandList, args: &IndirectArgs<'_>) {
        draw::dispatch_mesh_indirect(list, args);
    }

    fn write_timestamp(list: &mut command::CommandList, pool: &query::QueryPool, index: u32) {
        query::write_timestamp(list, pool, index);
    }

    fn begin_query(list: &mut command::CommandList, pool: &query::QueryPool, index: u32) {
        query::begin_query(list, pool, index);
    }

    fn end_query(list: &mut command::CommandList, pool: &query::QueryPool, index: u32) {
        query::end_query(list, pool, index);
    }

    fn resolve_queries(
        list: &mut command::CommandList,
        pool: &query::QueryPool,
        first: u32,
        count: u32,
        dst: &crate::Buffer,
        offset: u64,
    ) {
        query::resolve_queries(list, pool, first, count, dst, offset);
    }

    fn set_predication(
        list: &mut command::CommandList,
        predicate: Option<(&crate::Buffer, u64, PredicateSkip)>,
    ) {
        query::set_predication(list, predicate);
    }

    fn build_sizes(
        list: &mut command::CommandList,
        inputs: &AccelerationStructureInputs<'_>,
        flags: BuildFlags,
    ) -> AccelerationStructureSizes {
        raytracing::build_sizes(list, inputs, flags)
    }

    fn build_acceleration_structure(
        list: &mut command::CommandList,
        build: &BuildAccelerationStructure<'_>,
    ) {
        raytracing::build(list, build);
    }

    fn copy_acceleration_structure(
        list: &mut command::CommandList,
        src: &crate::AccelerationStructure,
        dst: &crate::AccelerationStructure,
        mode: AccelerationStructureCopy,
    ) {
        raytracing::copy(list, src, dst, mode);
    }

    fn write_compacted_sizes(
        list: &mut command::CommandList,
        structures: &[&crate::AccelerationStructure],
        dst: &crate::Buffer,
        offset: u64,
    ) {
        raytracing::write_compacted_sizes(list, structures, dst, offset);
    }

    fn begin_event(list: &mut command::CommandList, name: &str) {
        diag::begin_event(list, name);
    }

    fn end_event(list: &mut command::CommandList) {
        diag::end_event(list);
    }

    fn set_marker(list: &mut command::CommandList, name: &str) {
        diag::set_marker(list, name);
    }
}

impl crate::contract::Swapchain for Backend {
    fn acquire(swapchain: &mut swapchain::Swapchain, timeout: Option<Duration>) -> Result<u32> {
        swapchain.acquire(timeout)
    }

    fn present(swapchain: &mut swapchain::Swapchain) -> Result<PresentStatus> {
        swapchain.present()
    }

    fn resize(swapchain: &mut swapchain::Swapchain, width: u32, height: u32) -> Result<()> {
        swapchain.resize(width, height)
    }

    fn set_present_mode(swapchain: &mut swapchain::Swapchain, mode: PresentMode) {
        swapchain.set_present_mode(mode);
    }

    fn supports_color_space(swapchain: &swapchain::Swapchain, color_space: ColorSpace) -> bool {
        swapchain.supports_color_space(color_space)
    }

    fn set_color_space(
        swapchain: &mut swapchain::Swapchain,
        color_space: ColorSpace,
    ) -> Result<()> {
        swapchain.set_color_space(color_space)
    }

    fn set_hdr_metadata(
        swapchain: &mut swapchain::Swapchain,
        metadata: Option<&HdrMetadata>,
    ) -> Result<()> {
        swapchain.set_hdr_metadata(metadata)
    }

    fn containing_output(swapchain: &swapchain::Swapchain) -> Result<OutputInfo> {
        swapchain.containing_output()
    }

    fn frame_statistics(swapchain: &swapchain::Swapchain) -> Result<Option<FrameStatistics>> {
        swapchain.frame_statistics()
    }

    fn size(swapchain: &swapchain::Swapchain) -> (u32, u32) {
        swapchain.size()
    }

    fn format(swapchain: &swapchain::Swapchain) -> Format {
        swapchain.format()
    }

    fn color_space(swapchain: &swapchain::Swapchain) -> ColorSpace {
        swapchain.color_space()
    }

    fn textures(swapchain: &swapchain::Swapchain) -> &[crate::Texture] {
        swapchain.textures()
    }

    fn views(swapchain: &swapchain::Swapchain) -> &[crate::RenderTargetView] {
        swapchain.views()
    }
}
