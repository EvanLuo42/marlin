//! Placeholder for platforms whose backend is not written yet: [`Instance`] creation fails with
//! `Unsupported`, so no other object can exist.

use core::ptr::NonNull;
use core::time::Duration;

use crate::contract::{
    Commands, Device, Fences, Instance, Queue, Queues, Resources, Rhi, Swapchain, Types,
};
use crate::{
    AdapterInfo, Buffer, BufferBarrier, BufferDesc, BufferTextureCopy, BufferViewDesc,
    ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc, Error, ErrorKind, Features, Format,
    GraphicsPipelineDesc, HeapDesc, IndexFormat, IndirectArgs, InstanceDesc, MemoryRequirements,
    MeshPipelineDesc, PresentMode, PresentStatus, QueryPoolDesc, QueueType, Rect, RenderPassDesc,
    RenderTargetView, RenderTargetViewDesc, Result, SamplerDesc, SubmitDesc, SurfaceHandle,
    SwapchainDesc, Texture, TextureBarrier, TextureCopy, TextureDesc, TextureViewDesc, Viewport,
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
}

impl Rhi for Backend {}

impl Instance for Backend {
    fn create_instance(_: &InstanceDesc) -> Result<Never> {
        Err(Error::from_kind(ErrorKind::Unsupported))
    }

    fn adapter(instance: &Never, _: u32) -> Option<AdapterInfo> {
        match *instance {}
    }

    fn create_device(instance: &Never, _: &DeviceDesc) -> Result<(Never, Queues<Never>)> {
        match *instance {}
    }
}

impl Device for Backend {
    fn adapter_info(device: &Never) -> &AdapterInfo {
        match *device {}
    }

    fn features(device: &Never) -> Features {
        match *device {}
    }

    fn create_fence(device: &Never, _: u64) -> Result<Never> {
        match *device {}
    }

    fn create_command_pool(device: &Never, _: QueueType) -> Result<Never> {
        match *device {}
    }

    fn create_command_list(device: &Never, _: &Never) -> Result<Never> {
        match *device {}
    }

    unsafe fn create_swapchain(
        device: &Never,
        _: &Never,
        _: SurfaceHandle,
        _: &SwapchainDesc,
    ) -> Result<Never> {
        match *device {}
    }

    fn validation_errors(device: &Never) -> u64 {
        match *device {}
    }

    fn create_buffer(device: &Never, _: &BufferDesc) -> Result<Never> {
        match *device {}
    }

    fn create_texture(device: &Never, _: &TextureDesc) -> Result<Never> {
        match *device {}
    }

    fn create_render_target_view(
        device: &Never,
        _: &Texture,
        _: &RenderTargetViewDesc,
    ) -> Result<Never> {
        match *device {}
    }

    fn create_depth_stencil_view(
        device: &Never,
        _: &Texture,
        _: &DepthStencilViewDesc,
    ) -> Result<Never> {
        match *device {}
    }

    fn create_texture_view(device: &Never, _: &Texture, _: &TextureViewDesc) -> Result<Never> {
        match *device {}
    }

    fn create_buffer_view(device: &Never, _: &Buffer, _: &BufferViewDesc) -> Result<Never> {
        match *device {}
    }

    fn create_sampler(device: &Never, _: &SamplerDesc) -> Result<Never> {
        match *device {}
    }

    fn create_graphics_pipeline(device: &Never, _: &GraphicsPipelineDesc<'_>) -> Result<Never> {
        match *device {}
    }

    fn create_compute_pipeline(device: &Never, _: &ComputePipelineDesc<'_>) -> Result<Never> {
        match *device {}
    }

    fn create_mesh_pipeline(device: &Never, _: &MeshPipelineDesc<'_>) -> Result<Never> {
        match *device {}
    }

    fn create_query_pool(device: &Never, _: &QueryPoolDesc) -> Result<Never> {
        match *device {}
    }

    fn create_heap(device: &Never, _: &HeapDesc) -> Result<Never> {
        match *device {}
    }

    fn buffer_requirements(device: &Never, _: &BufferDesc) -> MemoryRequirements {
        match *device {}
    }

    fn texture_requirements(device: &Never, _: &TextureDesc) -> MemoryRequirements {
        match *device {}
    }

    fn create_placed_buffer(device: &Never, _: &Never, _: u64, _: &BufferDesc) -> Result<Never> {
        match *device {}
    }

    fn create_placed_texture(device: &Never, _: &Never, _: u64, _: &TextureDesc) -> Result<Never> {
        match *device {}
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
}

impl Queue for Backend {
    unsafe fn submit(queue: &Never, _: &SubmitDesc<'_>) -> Result<()> {
        match *queue {}
    }

    fn timestamp_frequency(queue: &Never) -> Result<u64> {
        match *queue {}
    }

    fn wait_idle(queue: &Never) -> Result<()> {
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

    fn texture_barriers(list: &mut Never, _: &[TextureBarrier<'_>]) {
        match *list {}
    }

    fn buffer_barriers(list: &mut Never, _: &[BufferBarrier<'_>]) {
        match *list {}
    }

    fn begin_render_pass(list: &mut Never, _: &RenderPassDesc<'_>) {
        match *list {}
    }

    fn end_render_pass(list: &mut Never) {
        match *list {}
    }

    fn copy_buffer(list: &mut Never, _: &Buffer, _: u64, _: &Buffer, _: u64, _: u64) {
        match *list {}
    }

    fn copy_buffer_to_texture(list: &mut Never, _: &Buffer, _: &Texture, _: &BufferTextureCopy) {
        match *list {}
    }

    fn copy_texture_to_buffer(list: &mut Never, _: &Texture, _: &Buffer, _: &BufferTextureCopy) {
        match *list {}
    }

    fn set_graphics_pipeline(list: &mut Never, _: &Never) {
        match *list {}
    }

    fn set_compute_pipeline(list: &mut Never, _: &Never) {
        match *list {}
    }

    fn push_constants(list: &mut Never, _: u32, _: &[u32]) {
        match *list {}
    }

    fn set_viewport(list: &mut Never, _: &Viewport) {
        match *list {}
    }

    fn set_scissor(list: &mut Never, _: &Rect) {
        match *list {}
    }

    fn set_stencil_reference(list: &mut Never, _: u8) {
        match *list {}
    }

    fn set_index_buffer(list: &mut Never, _: &Buffer, _: u64, _: IndexFormat) {
        match *list {}
    }

    fn draw(list: &mut Never, _: u32, _: u32, _: u32, _: u32) {
        match *list {}
    }

    fn draw_indexed(list: &mut Never, _: u32, _: u32, _: u32, _: i32, _: u32) {
        match *list {}
    }

    fn dispatch(list: &mut Never, _: u32, _: u32, _: u32) {
        match *list {}
    }

    fn set_mesh_pipeline(list: &mut Never, _: &Never) {
        match *list {}
    }

    fn dispatch_mesh(list: &mut Never, _: u32, _: u32, _: u32) {
        match *list {}
    }

    fn draw_indirect(list: &mut Never, _: &IndirectArgs<'_>) {
        match *list {}
    }

    fn draw_indexed_indirect(list: &mut Never, _: &IndirectArgs<'_>) {
        match *list {}
    }

    fn dispatch_indirect(list: &mut Never, _: &IndirectArgs<'_>) {
        match *list {}
    }

    fn dispatch_mesh_indirect(list: &mut Never, _: &IndirectArgs<'_>) {
        match *list {}
    }

    fn write_timestamp(list: &mut Never, _: &Never, _: u32) {
        match *list {}
    }

    fn resolve_queries(list: &mut Never, _: &Never, _: u32, _: u32, _: &Buffer, _: u64) {
        match *list {}
    }

    fn begin_event(list: &mut Never, _: &str) {
        match *list {}
    }

    fn end_event(list: &mut Never) {
        match *list {}
    }

    fn set_marker(list: &mut Never, _: &str) {
        match *list {}
    }

    fn copy_texture(list: &mut Never, _: &Texture, _: &Texture, _: &TextureCopy) {
        match *list {}
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

    fn size(swapchain: &Never) -> (u32, u32) {
        match *swapchain {}
    }

    fn format(swapchain: &Never) -> Format {
        match *swapchain {}
    }

    fn textures(swapchain: &Never) -> &[Texture] {
        match *swapchain {}
    }

    fn views(swapchain: &Never) -> &[RenderTargetView] {
        match *swapchain {}
    }
}
