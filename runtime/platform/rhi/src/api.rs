//! Public types: thin wrappers forwarding to the selected backend.
//!
//! Descriptions are validated here, once for every backend: invalid ones return `InvalidInput`;
//! out-of-range recording arguments (copies, push constants) panic, as slice indexing does.

use core::ptr::NonNull;
use core::time::Duration;

use crate::backend::imp::Backend;
use crate::contract::{
    Commands, Device as _, Fences, Instance as _, Queue as _, Resources, Swapchain as _, Types,
};
use crate::{
    AdapterInfo, BufferBarrier, BufferDesc, BufferTextureCopy, BufferUsage, BufferViewDesc,
    BufferViewKind, ClearValue, ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc, Error,
    ErrorKind, Features, Format, GraphicsPipelineDesc, IndexFormat, InstanceDesc,
    MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MAX_UNIFORM_SIZE, MemoryLocation, PresentMode,
    PresentStatus, Rect, RenderPassDesc, RenderTargetViewDesc, Result, SamplerDesc,
    SubresourceRange, SurfaceHandle, SwapchainDesc, TEXTURE_OFFSET_ALIGNMENT,
    TEXTURE_ROW_PITCH_ALIGNMENT, TextureBarrier, TextureDesc, TextureUsage, TextureViewDesc,
    TextureViewDimension, UNIFORM_ALIGNMENT, Viewport,
};

fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

fn ensure(condition: bool) -> Result<()> {
    if condition { Ok(()) } else { Err(invalid()) }
}

/// Largest texture dimension (D3D12's limit for 2D textures).
const MAX_TEXTURE_SIZE: u32 = 16384;
const MAX_TEXTURE_LAYERS: u32 = 2048;

/// The API entry point (DXGI factory, `VkInstance`). Enumerates adapters and creates devices.
pub struct Instance {
    raw: <Backend as Types>::Instance,
}

impl Instance {
    pub fn new(desc: &InstanceDesc) -> Result<Self> {
        Ok(Self {
            raw: Backend::create_instance(desc)?,
        })
    }

    /// Every adapter, highest performance first; software adapters last.
    pub fn adapters(&self) -> impl Iterator<Item = AdapterInfo> + '_ {
        (0..).map_while(|i| Backend::adapter(&self.raw, i))
    }

    /// Fails with `Unsupported` if the adapter lacks the baseline (D3D12: feature level 12_0,
    /// shader model 6.6, resource binding tier 3).
    pub fn create_device(&self, desc: &DeviceDesc) -> Result<Device> {
        let (raw, queue) = Backend::create_device(&self.raw, desc)?;
        Ok(Device {
            queue: Queue { raw: queue },
            raw,
        })
    }
}

/// A GPU. Owns the graphics queue and the bindless tables. Dropping it waits for the queue to go
/// idle.
pub struct Device {
    queue: Queue,
    raw: <Backend as Types>::Device,
}

impl Device {
    #[inline]
    pub fn adapter_info(&self) -> &AdapterInfo {
        Backend::adapter_info(&self.raw)
    }

    #[inline]
    pub fn features(&self) -> Features {
        Backend::features(&self.raw)
    }

    /// The graphics queue (also does compute and copies).
    #[inline]
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    pub fn create_fence(&self, initial_value: u64) -> Result<Fence> {
        Ok(Fence {
            raw: Backend::create_fence(&self.raw, initial_value)?,
        })
    }

    /// A command pool for the graphics queue.
    pub fn create_command_pool(&self) -> Result<CommandPool> {
        Ok(CommandPool {
            raw: Backend::create_command_pool(&self.raw)?,
        })
    }

    /// A command list whose memory comes from `pool` (and from whichever pool it is begun with
    /// later).
    pub fn create_command_list(&self, pool: &CommandPool) -> Result<CommandList> {
        Ok(CommandList {
            raw: Backend::create_command_list(&self.raw, &pool.raw)?,
        })
    }

    /// A swapchain presenting from the graphics queue.
    ///
    /// # Safety
    /// `surface` must be a live window, and it must outlive the swapchain.
    pub unsafe fn create_swapchain(
        &self,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<Swapchain> {
        Ok(Swapchain {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::create_swapchain(&self.raw, &self.queue.raw, surface, desc)? },
        })
    }

    /// Errors (and corruption reports) the validation layer has logged so far. 0 without
    /// validation. Tests assert this stays 0.
    ///
    /// D3D12 returns the same device for every device created on one adapter in a process, so
    /// such `Device`s share one message queue: a message is counted by whichever drains it first.
    #[inline]
    pub fn validation_errors(&self) -> u64 {
        Backend::validation_errors(&self.raw)
    }

    pub fn create_buffer(&self, desc: &BufferDesc) -> Result<Buffer> {
        ensure(desc.size > 0)?;
        ensure(
            !desc.usage.contains(BufferUsage::SHADER_WRITE)
                || desc.memory == MemoryLocation::GpuOnly,
        )?;
        Ok(Buffer {
            raw: Backend::create_buffer(&self.raw, desc)?,
            desc: *desc,
        })
    }

    pub fn create_texture(&self, desc: &TextureDesc) -> Result<Texture> {
        validate_texture(desc)?;
        Ok(Texture {
            raw: Backend::create_texture(&self.raw, desc)?,
            desc: *desc,
        })
    }

    /// A color target view of one mip and layer. Requires [`TextureUsage::RENDER_TARGET`].
    pub fn create_render_target_view(
        &self,
        texture: &Texture,
        desc: &RenderTargetViewDesc,
    ) -> Result<RenderTargetView> {
        let t = &texture.desc;
        ensure(t.usage.contains(TextureUsage::RENDER_TARGET))?;
        ensure(desc.mip < t.mip_levels && desc.layer < t.array_layers)?;
        let format = desc.format.unwrap_or(t.format);
        ensure(format.linear() == t.format.linear())?;
        let desc = RenderTargetViewDesc {
            format: Some(format),
            ..*desc
        };
        Ok(RenderTargetView {
            raw: Backend::create_render_target_view(&self.raw, texture, &desc)?,
        })
    }

    /// A depth target view of one mip and layer. Requires [`TextureUsage::DEPTH_STENCIL`].
    pub fn create_depth_stencil_view(
        &self,
        texture: &Texture,
        desc: &DepthStencilViewDesc,
    ) -> Result<DepthStencilView> {
        let t = &texture.desc;
        ensure(t.usage.contains(TextureUsage::DEPTH_STENCIL))?;
        ensure(desc.mip < t.mip_levels && desc.layer < t.array_layers)?;
        Ok(DepthStencilView {
            raw: Backend::create_depth_stencil_view(&self.raw, texture, desc)?,
        })
    }

    /// A bindless view of a texture, for shaders to reach through its
    /// [`index`](ShaderView::index).
    pub fn create_texture_view(
        &self,
        texture: &Texture,
        desc: &TextureViewDesc,
    ) -> Result<ShaderView> {
        let t = &texture.desc;
        ensure(t.usage.contains(match desc.writable {
            true => TextureUsage::STORAGE,
            false => TextureUsage::SAMPLED,
        }))?;
        let r = desc.range;
        ensure(r.base_mip < t.mip_levels && r.base_layer < t.array_layers)?;
        let mip_count = match r.mip_count {
            u32::MAX => t.mip_levels - r.base_mip,
            n => n,
        };
        let layer_count = match r.layer_count {
            u32::MAX => match desc.dimension {
                TextureViewDimension::D2 => 1,
                TextureViewDimension::D2Array => t.array_layers - r.base_layer,
                TextureViewDimension::Cube => 6,
            },
            n => n,
        };
        ensure(mip_count > 0 && r.base_mip + mip_count <= t.mip_levels)?;
        ensure(layer_count > 0 && r.base_layer + layer_count <= t.array_layers)?;
        ensure(!desc.writable || mip_count == 1)?;
        match desc.dimension {
            TextureViewDimension::D2 => ensure(layer_count == 1)?,
            TextureViewDimension::D2Array => {}
            TextureViewDimension::Cube => ensure(
                !desc.writable && r.base_layer == 0 && layer_count == 6 && t.width == t.height,
            )?,
        }
        let format = match desc.format {
            Some(f) => {
                ensure(f.linear() == t.format.linear())?;
                f
            }
            None if t.format.is_depth() => Format::R32Float,
            None => t.format,
        };
        let desc = TextureViewDesc {
            format: Some(format),
            range: SubresourceRange {
                base_mip: r.base_mip,
                mip_count,
                base_layer: r.base_layer,
                layer_count,
            },
            ..*desc
        };
        Ok(ShaderView {
            raw: Backend::create_texture_view(&self.raw, texture, &desc)?,
        })
    }

    /// A bindless view of a buffer range, for shaders to reach through its
    /// [`index`](ShaderView::index).
    pub fn create_buffer_view(&self, buffer: &Buffer, desc: &BufferViewDesc) -> Result<ShaderView> {
        let b = &buffer.desc;
        ensure(desc.offset < b.size)?;
        let size = match desc.size {
            u64::MAX => b.size - desc.offset,
            n => n,
        };
        ensure(size > 0 && size <= b.size - desc.offset)?;
        if desc.writable {
            ensure(b.usage.contains(BufferUsage::SHADER_WRITE))?;
        }
        match desc.kind {
            BufferViewKind::Uniform => {
                ensure(b.usage.contains(BufferUsage::UNIFORM) && !desc.writable)?;
                ensure(desc.offset.is_multiple_of(UNIFORM_ALIGNMENT) && size <= MAX_UNIFORM_SIZE)?;
            }
            BufferViewKind::Structured { stride } => {
                ensure(stride > 0 && stride <= 2048)?;
                ensure(desc.offset.is_multiple_of(stride as u64))?;
                ensure(size.is_multiple_of(stride as u64))?;
            }
            BufferViewKind::Raw => {
                ensure(desc.offset.is_multiple_of(4) && size.is_multiple_of(4))?;
            }
        }
        if !desc.writable && desc.kind != BufferViewKind::Uniform {
            ensure(b.usage.contains(BufferUsage::SHADER_READ))?;
        }
        let desc = BufferViewDesc { size, ..*desc };
        Ok(ShaderView {
            raw: Backend::create_buffer_view(&self.raw, buffer, &desc)?,
        })
    }

    /// A bindless sampler, for shaders to reach through its [`index`](Sampler::index).
    pub fn create_sampler(&self, desc: &SamplerDesc) -> Result<Sampler> {
        ensure((1..=16).contains(&desc.max_anisotropy))?;
        ensure(desc.min_lod <= desc.max_lod)?;
        Ok(Sampler {
            raw: Backend::create_sampler(&self.raw, desc)?,
        })
    }

    pub fn create_graphics_pipeline(
        &self,
        desc: &GraphicsPipelineDesc<'_>,
    ) -> Result<GraphicsPipeline> {
        ensure(desc.color_targets.len() <= MAX_COLOR_ATTACHMENTS)?;
        ensure(desc.depth.is_none_or(|d| d.format.is_depth()))?;
        ensure(
            desc.color_targets
                .iter()
                .all(|t| !t.format.is_depth() && !t.format.is_compressed()),
        )?;
        ensure(!desc.vertex.code.is_empty() && desc.pixel.is_none_or(|p| !p.code.is_empty()))?;
        Ok(GraphicsPipeline {
            raw: Backend::create_graphics_pipeline(&self.raw, desc)?,
        })
    }

    pub fn create_compute_pipeline(
        &self,
        desc: &ComputePipelineDesc<'_>,
    ) -> Result<ComputePipeline> {
        ensure(!desc.shader.code.is_empty())?;
        Ok(ComputePipeline {
            raw: Backend::create_compute_pipeline(&self.raw, desc)?,
        })
    }
}

fn validate_texture(d: &TextureDesc) -> Result<()> {
    ensure((1..=MAX_TEXTURE_SIZE).contains(&d.width))?;
    ensure((1..=MAX_TEXTURE_SIZE).contains(&d.height))?;
    ensure((1..=MAX_TEXTURE_LAYERS).contains(&d.array_layers))?;
    let max_mips = 32 - d.width.max(d.height).leading_zeros();
    ensure((1..=max_mips).contains(&d.mip_levels))?;
    ensure(d.usage != TextureUsage::default())?;
    let f = d.format;
    let attachment_or_storage =
        TextureUsage::RENDER_TARGET | TextureUsage::DEPTH_STENCIL | TextureUsage::STORAGE;
    if f.is_depth() {
        ensure(!d.usage.contains(TextureUsage::RENDER_TARGET))?;
        ensure(!d.usage.contains(TextureUsage::STORAGE))?;
    } else {
        ensure(!d.usage.contains(TextureUsage::DEPTH_STENCIL))?;
    }
    if f.is_compressed() {
        ensure(d.usage.bits() & attachment_or_storage.bits() == 0)?;
        ensure(d.width.is_multiple_of(4) && d.height.is_multiple_of(4))?;
    }
    if f.is_srgb() {
        ensure(!d.usage.contains(TextureUsage::STORAGE))?;
    }
    match d.clear_value {
        None => Ok(()),
        Some(ClearValue::Color(_)) => ensure(d.usage.contains(TextureUsage::RENDER_TARGET)),
        Some(ClearValue::Depth(_)) => ensure(d.usage.contains(TextureUsage::DEPTH_STENCIL)),
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.queue.wait_idle();
    }
}

/// A GPU queue: executes command lists in submission order.
pub struct Queue {
    raw: <Backend as Types>::Queue,
}

impl Queue {
    /// Executes `lists` in order, then sets `signal.0` to `signal.1` once they finish.
    ///
    /// # Safety
    /// Until the GPU finishes these lists (observe it through the signalled fence): every
    /// resource and view they use stays alive, and neither the lists nor their pools are reset or
    /// dropped. Shaders only reach live views through the bindless tables.
    pub unsafe fn submit(
        &self,
        lists: &[&CommandList],
        signal: Option<(&Fence, u64)>,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::submit(&self.raw, lists, signal) }
    }

    /// Blocks until everything submitted so far has finished.
    pub fn wait_idle(&self) -> Result<()> {
        Backend::wait_idle(&self.raw)
    }
}

/// A monotonically increasing 64-bit counter shared by CPU and GPU: D3D12 fence, Vulkan timeline
/// semaphore, Metal shared event.
pub struct Fence {
    pub(crate) raw: <Backend as Types>::Fence,
}

impl Fence {
    #[inline]
    pub fn completed_value(&self) -> u64 {
        Backend::completed_value(&self.raw)
    }

    /// Blocks until the fence reaches `value`. Returns `false` if `timeout` passed first.
    pub fn wait(&self, value: u64, timeout: Option<Duration>) -> Result<bool> {
        Backend::wait(&self.raw, value, timeout)
    }

    /// Sets the value from the CPU.
    pub fn signal(&self, value: u64) -> Result<()> {
        Backend::signal(&self.raw, value)
    }
}

/// Memory that command lists record into (D3D12 command allocator, `VkCommandPool`). One list
/// records from a pool at a time; reset it once the GPU is done with everything recorded.
pub struct CommandPool {
    raw: <Backend as Types>::CommandPool,
}

impl CommandPool {
    /// Frees everything recorded from this pool for reuse.
    ///
    /// # Safety
    /// The GPU has finished executing every command list recorded from this pool since the last
    /// reset.
    pub unsafe fn reset(&mut self) -> Result<()> {
        // SAFETY: forwarded from the caller.
        unsafe { Backend::reset_pool(&mut self.raw) }
    }
}

/// A command list. [`begin`](Self::begin) to record, [`end`](Self::end), then submit.
///
/// Each recording starts with the bindless tables bound and the push constants zeroed; pipelines,
/// viewports, scissors and index buffers must be set before drawing.
#[repr(transparent)]
pub struct CommandList {
    pub(crate) raw: <Backend as Types>::CommandList,
}

impl CommandList {
    /// Starts recording into `pool`, discarding what was recorded before. The previous recording
    /// must not be pending on the GPU.
    pub fn begin(&mut self, pool: &mut CommandPool) -> Result<()> {
        Backend::begin(&mut self.raw, &mut pool.raw)
    }

    /// Finishes recording. Reports errors the API detected while recording.
    pub fn end(&mut self) -> Result<()> {
        Backend::end(&mut self.raw)
    }

    pub fn texture_barriers(&mut self, barriers: &[TextureBarrier<'_>]) {
        Backend::texture_barriers(&mut self.raw, barriers);
    }

    pub fn buffer_barriers(&mut self, barriers: &[BufferBarrier<'_>]) {
        Backend::buffer_barriers(&mut self.raw, barriers);
    }

    /// Binds render targets and applies their load ops. No barriers or copies are allowed until
    /// [`end_render_pass`](Self::end_render_pass).
    pub fn begin_render_pass(&mut self, desc: &RenderPassDesc<'_>) {
        assert!(desc.color.len() <= MAX_COLOR_ATTACHMENTS);
        Backend::begin_render_pass(&mut self.raw, desc);
    }

    pub fn end_render_pass(&mut self) {
        Backend::end_render_pass(&mut self.raw);
    }

    /// Copies `size` bytes. Panics if a range is out of bounds or a usage is missing.
    pub fn copy_buffer(
        &mut self,
        src: &Buffer,
        src_offset: u64,
        dst: &Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        assert!(src.desc.usage.contains(BufferUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        assert!(
            src_offset
                .checked_add(size)
                .is_some_and(|e| e <= src.desc.size)
        );
        assert!(
            dst_offset
                .checked_add(size)
                .is_some_and(|e| e <= dst.desc.size)
        );
        Backend::copy_buffer(&mut self.raw, src, src_offset, dst, dst_offset, size);
    }

    /// Copies buffer rows into a texture region. Panics if the region is invalid (see
    /// [`BufferTextureCopy`]) or a usage is missing.
    pub fn copy_buffer_to_texture(
        &mut self,
        src: &Buffer,
        dst: &Texture,
        region: &BufferTextureCopy,
    ) {
        assert!(src.desc.usage.contains(BufferUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(TextureUsage::COPY_DST));
        check_copy(src, dst, region);
        Backend::copy_buffer_to_texture(&mut self.raw, src, dst, region);
    }

    /// Copies a texture region into buffer rows. Panics like
    /// [`copy_buffer_to_texture`](Self::copy_buffer_to_texture).
    pub fn copy_texture_to_buffer(
        &mut self,
        src: &Texture,
        dst: &Buffer,
        region: &BufferTextureCopy,
    ) {
        assert!(src.desc.usage.contains(TextureUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        check_copy(dst, src, region);
        Backend::copy_texture_to_buffer(&mut self.raw, src, dst, region);
    }

    pub fn set_graphics_pipeline(&mut self, pipeline: &GraphicsPipeline) {
        Backend::set_graphics_pipeline(&mut self.raw, &pipeline.raw);
    }

    pub fn set_compute_pipeline(&mut self, pipeline: &ComputePipeline) {
        Backend::set_compute_pipeline(&mut self.raw, &pipeline.raw);
    }

    /// Writes `data` at word `offset` of the push constants of the pipeline kind bound last
    /// (graphics if none). Panics past [`MAX_PUSH_CONSTANTS`] words.
    pub fn push_constants(&mut self, offset: u32, data: &[u32]) {
        assert!(
            (offset as usize)
                .checked_add(data.len())
                .is_some_and(|end| end <= MAX_PUSH_CONSTANTS as usize)
        );
        Backend::push_constants(&mut self.raw, offset, data);
    }

    pub fn set_viewport(&mut self, viewport: &Viewport) {
        Backend::set_viewport(&mut self.raw, viewport);
    }

    pub fn set_scissor(&mut self, rect: &Rect) {
        Backend::set_scissor(&mut self.raw, rect);
    }

    /// Panics without [`BufferUsage::INDEX`] or with an offset past the end or misaligned.
    pub fn set_index_buffer(&mut self, buffer: &Buffer, offset: u64, format: IndexFormat) {
        assert!(buffer.desc.usage.contains(BufferUsage::INDEX));
        let size = match format {
            IndexFormat::U16 => 2,
            IndexFormat::U32 => 4,
        };
        assert!(offset < buffer.desc.size && offset.is_multiple_of(size));
        Backend::set_index_buffer(&mut self.raw, buffer, offset, format);
    }

    /// Shaders see vertex indices `first_vertex..` and instance indices from 0: the instance
    /// offset is not visible to D3D12 shaders, so pass it in push constants when needed.
    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        Backend::draw(
            &mut self.raw,
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        );
    }

    /// Shaders see vertex index `index + vertex_offset`; instances as in [`draw`](Self::draw).
    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        Backend::draw_indexed(
            &mut self.raw,
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        );
    }

    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        Backend::dispatch(&mut self.raw, x, y, z);
    }
}

fn check_copy(buffer: &Buffer, texture: &Texture, r: &BufferTextureCopy) {
    let d = &texture.desc;
    assert!(
        r.mip < d.mip_levels && r.layer < d.array_layers,
        "copy subresource out of range"
    );
    let (w, h) = d.mip_size(r.mip);
    let block = d.format.block_extent();
    // Block-compressed mips are stored whole blocks wide, even below 4 texels.
    let (w, h) = (w.next_multiple_of(block), h.next_multiple_of(block));
    assert!(
        r.x.checked_add(r.width).is_some_and(|e| e <= w)
            && r.y.checked_add(r.height).is_some_and(|e| e <= h)
            && r.width > 0
            && r.height > 0,
        "copy region outside the subresource"
    );
    assert!(
        [r.x, r.y, r.width, r.height]
            .iter()
            .all(|v| v.is_multiple_of(block)),
        "copy region not block-aligned"
    );
    if d.format.is_depth() {
        assert!(
            r.x == 0 && r.y == 0 && (r.width, r.height) == (w, h),
            "depth copies cover the whole subresource"
        );
    }
    assert!(
        r.bytes_per_row.is_multiple_of(TEXTURE_ROW_PITCH_ALIGNMENT)
            && r.buffer_offset.is_multiple_of(TEXTURE_OFFSET_ALIGNMENT),
        "copy rows misaligned"
    );
    let row = (r.width / block) as u64 * d.format.block_size() as u64;
    assert!(r.bytes_per_row as u64 >= row, "copy rows overlap");
    let end = r.buffer_offset + (r.height / block - 1) as u64 * r.bytes_per_row as u64 + row;
    assert!(end <= buffer.desc.size, "copy past the end of the buffer");
}

/// A linear GPU buffer.
pub struct Buffer {
    pub(crate) raw: <Backend as Types>::Buffer,
    pub(crate) desc: BufferDesc,
}

impl Buffer {
    #[inline]
    pub fn desc(&self) -> &BufferDesc {
        &self.desc
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.desc.size
    }

    /// The CPU address of an [`Upload`](MemoryLocation::Upload) or
    /// [`Readback`](MemoryLocation::Readback) buffer, valid for the buffer's life.
    #[inline]
    pub fn mapped(&self) -> Option<NonNull<u8>> {
        Backend::buffer_mapped(&self.raw)
    }

    /// Copies `data` into a mapped buffer at `offset`. Panics if unmapped or out of bounds.
    ///
    /// # Safety
    /// The GPU is not accessing those bytes (no pending submission uses them).
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        let base = self.mapped().expect("buffer is not mapped");
        assert!(
            offset
                .checked_add(data.len() as u64)
                .is_some_and(|e| e <= self.desc.size)
        );
        // SAFETY: in bounds of the mapping; the caller rules out GPU access.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                base.as_ptr().add(offset as usize),
                data.len(),
            );
        }
    }

    /// Copies from a mapped buffer at `offset` into `out`. Panics if unmapped or out of bounds.
    ///
    /// # Safety
    /// The GPU has finished writing those bytes and is not accessing them.
    pub unsafe fn read(&self, offset: u64, out: &mut [u8]) {
        let base = self.mapped().expect("buffer is not mapped");
        assert!(
            offset
                .checked_add(out.len() as u64)
                .is_some_and(|e| e <= self.desc.size)
        );
        // SAFETY: as in `write`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                base.as_ptr().add(offset as usize),
                out.as_mut_ptr(),
                out.len(),
            );
        }
    }
}

/// A GPU texture.
pub struct Texture {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no textures")
    )]
    pub(crate) raw: <Backend as Types>::Texture,
    pub(crate) desc: TextureDesc,
}

impl Texture {
    #[inline]
    pub fn desc(&self) -> &TextureDesc {
        &self.desc
    }
}

/// A view of a texture as a color render target: one of a swapchain's, or one created from a
/// texture (kept alive no longer than the texture).
pub struct RenderTargetView {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no views")
    )]
    pub(crate) raw: <Backend as Types>::RenderTargetView,
}

/// A view of a texture as a depth target.
pub struct DepthStencilView {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no views")
    )]
    pub(crate) raw: <Backend as Types>::DepthStencilView,
}

/// A slot in the bindless resource table holding a buffer or texture view. Shaders reach it by
/// [`index`](Self::index) (`ResourceDescriptorHeap[index]` in HLSL terms). Dropping it frees the
/// slot for reuse, so it must outlive the GPU work that uses it.
pub struct ShaderView {
    raw: <Backend as Types>::ShaderView,
}

impl ShaderView {
    #[inline]
    pub fn index(&self) -> u32 {
        Backend::shader_view_index(&self.raw)
    }
}

/// A slot in the bindless sampler table (`SamplerDescriptorHeap[index]`).
pub struct Sampler {
    raw: <Backend as Types>::Sampler,
}

impl Sampler {
    #[inline]
    pub fn index(&self) -> u32 {
        Backend::sampler_index(&self.raw)
    }
}

/// Compiled shaders plus fixed-function state, for draws.
pub struct GraphicsPipeline {
    raw: <Backend as Types>::GraphicsPipeline,
}

/// A compiled compute shader, for dispatches.
pub struct ComputePipeline {
    raw: <Backend as Types>::ComputePipeline,
}

/// The images a window presents, rotated by the presentation engine.
///
/// Per frame: [`acquire`](Self::acquire) an image, record work that moves it from
/// [`TextureState::PRESENT`](crate::TextureState::PRESENT) to a render target and back, submit,
/// then [`present`](Self::present). Dropping the swapchain waits for its queue to go idle.
pub struct Swapchain {
    raw: <Backend as Types>::Swapchain,
}

impl Swapchain {
    /// Blocks until the CPU may start the next frame (at most
    /// [`SwapchainDesc::max_frame_latency`] frames queued), then returns the index of the image to
    /// render. Call once per [`present`](Self::present).
    pub fn acquire(&mut self, timeout: Option<Duration>) -> Result<u32> {
        Backend::acquire(&mut self.raw, timeout)
    }

    /// Queues the acquired image for display once the work submitted before this call finishes.
    pub fn present(&mut self) -> Result<PresentStatus> {
        Backend::present(&mut self.raw)
    }

    /// Recreates the images at a new size (after a window resize). Waits for the GPU first, so
    /// command lists that used the old images must not be resubmitted.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        Backend::resize(&mut self.raw, width, height)
    }

    /// Takes effect at the next [`present`](Self::present).
    pub fn set_present_mode(&mut self, mode: PresentMode) {
        Backend::set_present_mode(&mut self.raw, mode);
    }

    #[inline]
    pub fn size(&self) -> (u32, u32) {
        Backend::size(&self.raw)
    }

    #[inline]
    pub fn format(&self) -> Format {
        Backend::format(&self.raw)
    }

    #[inline]
    pub fn image_count(&self) -> u32 {
        Backend::textures(&self.raw).len() as u32
    }

    /// Panics if `index >= image_count()`.
    #[inline]
    pub fn texture(&self, index: u32) -> &Texture {
        &Backend::textures(&self.raw)[index as usize]
    }

    /// The render target view of image `index`, in the swapchain's format (sRGB if requested).
    #[inline]
    pub fn view(&self, index: u32) -> &RenderTargetView {
        &Backend::views(&self.raw)[index as usize]
    }
}

macro_rules! opaque_debug {
    ($($ty:ident),*) => {$(
        impl core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    )*};
}

opaque_debug!(
    Instance,
    Queue,
    Fence,
    CommandPool,
    CommandList,
    RenderTargetView,
    DepthStencilView,
    GraphicsPipeline,
    ComputePipeline,
    Swapchain
);

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("adapter", &self.adapter_info().name())
            .field("features", &self.features())
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Buffer").field("desc", &self.desc).finish()
    }
}

impl core::fmt::Debug for Texture {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Texture").field("desc", &self.desc).finish()
    }
}

impl core::fmt::Debug for ShaderView {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ShaderView")
            .field("index", &self.index())
            .finish()
    }
}

impl core::fmt::Debug for Sampler {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sampler")
            .field("index", &self.index())
            .finish()
    }
}
