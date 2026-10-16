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
    BufferViewKind, ClearValue, ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc,
    DispatchIndirectArgs, DrawIndexedIndirectArgs, DrawIndirectArgs, Error, ErrorKind, Features,
    Format, GraphicsPipelineDesc, HeapDesc, HeapKind, IndexFormat, IndirectArgs, InstanceDesc,
    MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MAX_UNIFORM_SIZE, MemoryLocation,
    MemoryRequirements, MeshPipelineDesc, PresentMode, PresentStatus, QueryKind, QueryPoolDesc,
    QueueType, Rect, RenderPassDesc, RenderTargetViewDesc, Result, SamplerDesc, SubmitDesc,
    SubresourceRange, SurfaceHandle, SwapchainDesc, TEXTURE_OFFSET_ALIGNMENT,
    TEXTURE_ROW_PITCH_ALIGNMENT, TextureBarrier, TextureCopy, TextureDesc, TextureDimension,
    TextureUsage, TextureViewDesc, TextureViewDimension, UNIFORM_ALIGNMENT, Viewport,
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
const MAX_TEXTURE_3D_SIZE: u32 = 2048;

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
    /// shader model 6.6, resource binding tier 3, mesh shaders).
    pub fn create_device(&self, desc: &DeviceDesc) -> Result<Device> {
        let (raw, queues) = Backend::create_device(&self.raw, desc)?;
        Ok(Device {
            queue: Queue {
                raw: queues.graphics,
                ty: QueueType::Graphics,
            },
            compute: queues.compute.map(|raw| Queue {
                raw,
                ty: QueueType::Compute,
            }),
            copy: queues.copy.map(|raw| Queue {
                raw,
                ty: QueueType::Copy,
            }),
            raw,
        })
    }
}

/// A GPU. Owns its queues and the bindless tables. Dropping it waits for the queues to go idle.
pub struct Device {
    queue: Queue,
    compute: Option<Queue>,
    copy: Option<Queue>,
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

    /// The dedicated queue of type `ty` (async compute, copies for streaming), or `None` where
    /// the API has none: then the graphics queue does that work.
    #[inline]
    pub fn dedicated_queue(&self, ty: QueueType) -> Option<&Queue> {
        match ty {
            QueueType::Graphics => Some(&self.queue),
            QueueType::Compute => self.compute.as_ref(),
            QueueType::Copy => self.copy.as_ref(),
        }
    }

    /// The queue that runs `ty` work: the dedicated one, else the graphics queue.
    #[inline]
    pub fn queue_for(&self, ty: QueueType) -> &Queue {
        self.dedicated_queue(ty).unwrap_or(&self.queue)
    }

    pub fn create_fence(&self, initial_value: u64) -> Result<Fence> {
        Ok(Fence {
            raw: Backend::create_fence(&self.raw, initial_value)?,
        })
    }

    /// A command pool for the graphics queue.
    pub fn create_command_pool(&self) -> Result<CommandPool> {
        self.create_command_pool_for(QueueType::Graphics)
    }

    /// A command pool for lists of queue type `ty`. Without a dedicated queue of that type, the
    /// lists record for the graphics queue (they may only use `ty`'s commands all the same).
    pub fn create_command_pool_for(&self, ty: QueueType) -> Result<CommandPool> {
        let backend_ty = self.queue_for(ty).ty;
        Ok(CommandPool {
            raw: Backend::create_command_pool(&self.raw, backend_ty)?,
            ty,
            backend_ty,
        })
    }

    /// A command list whose memory comes from `pool` (and from whichever pool of the same type
    /// it is begun with later). It records `pool`'s queue type's commands.
    pub fn create_command_list(&self, pool: &CommandPool) -> Result<CommandList> {
        Ok(CommandList {
            raw: Backend::create_command_list(&self.raw, &pool.raw)?,
            ty: pool.ty,
            backend_ty: pool.backend_ty,
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
        validate_buffer(desc)?;
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
                TextureViewDimension::D2 | TextureViewDimension::D3 => 1,
                TextureViewDimension::D2Array => t.array_layers - r.base_layer,
                TextureViewDimension::Cube => 6,
            },
            n => n,
        };
        ensure(mip_count > 0 && r.base_mip + mip_count <= t.mip_levels)?;
        ensure(layer_count > 0 && r.base_layer + layer_count <= t.array_layers)?;
        ensure(!desc.writable || mip_count == 1)?;
        let volume = t.dimension == TextureDimension::D3;
        match desc.dimension {
            TextureViewDimension::D2 => ensure(layer_count == 1 && !volume)?,
            TextureViewDimension::D2Array => ensure(!volume)?,
            TextureViewDimension::Cube => ensure(
                !volume
                    && !desc.writable
                    && r.base_layer == 0
                    && layer_count == 6
                    && t.width == t.height,
            )?,
            TextureViewDimension::D3 => ensure(volume && layer_count == 1)?,
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
        ensure(desc.depth.is_none_or(|d| {
            d.format.is_depth() && (d.stencil.is_none() || d.format.has_stencil())
        }))?;
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

    /// A mesh shader pipeline. `Unsupported` without [`Features::mesh_shader`].
    pub fn create_mesh_pipeline(&self, desc: &MeshPipelineDesc<'_>) -> Result<MeshPipeline> {
        if !self.features().mesh_shader {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        ensure(desc.color_targets.len() <= MAX_COLOR_ATTACHMENTS)?;
        ensure(desc.depth.is_none_or(|d| {
            d.format.is_depth() && (d.stencil.is_none() || d.format.has_stencil())
        }))?;
        ensure(
            desc.color_targets
                .iter()
                .all(|t| !t.format.is_depth() && !t.format.is_compressed()),
        )?;
        ensure(!desc.mesh.code.is_empty())?;
        ensure(desc.amplification.is_none_or(|a| !a.code.is_empty()))?;
        ensure(desc.pixel.is_none_or(|p| !p.code.is_empty()))?;
        Ok(MeshPipeline {
            raw: Backend::create_mesh_pipeline(&self.raw, desc)?,
        })
    }

    /// `count` queries (1..=65536) of one kind.
    pub fn create_query_pool(&self, desc: &QueryPoolDesc) -> Result<QueryPool> {
        ensure((1..=65536).contains(&desc.count))?;
        if desc.kind == QueryKind::CopyTimestamp && !self.features().copy_queue_timestamps {
            return Err(Error::from_kind(ErrorKind::Unsupported));
        }
        Ok(QueryPool {
            raw: Backend::create_query_pool(&self.raw, desc)?,
            desc: *desc,
        })
    }

    /// Memory for placed resources ([`create_placed_buffer`](Self::create_placed_buffer),
    /// [`create_placed_texture`](Self::create_placed_texture)): pools of streamed textures and
    /// the render graph's transient resources, which alias each other.
    pub fn create_heap(&self, desc: &HeapDesc) -> Result<Heap> {
        ensure(desc.size > 0)?;
        ensure(desc.kind != HeapKind::Any || self.features().mixed_heaps)?;
        ensure(
            desc.memory == MemoryLocation::GpuOnly
                || matches!(desc.kind, HeapKind::Any | HeapKind::Buffers),
        )?;
        Ok(Heap {
            raw: Backend::create_heap(&self.raw, desc)?,
            desc: *desc,
        })
    }

    /// The size and alignment `desc` takes in a heap.
    pub fn buffer_requirements(&self, desc: &BufferDesc) -> MemoryRequirements {
        Backend::buffer_requirements(&self.raw, desc)
    }

    /// The size and alignment `desc` takes in a heap. `desc` must be valid for
    /// [`create_texture`](Self::create_texture).
    pub fn texture_requirements(&self, desc: &TextureDesc) -> Result<MemoryRequirements> {
        validate_texture(desc)?;
        Ok(Backend::texture_requirements(&self.raw, desc))
    }

    /// A buffer in `heap` at `offset` (aligned to its requirements). Placed resources may
    /// overlap: only one of the overlapping resources holds meaningful contents at a time, and a
    /// texture's first use after another resource used its memory starts from
    /// [`Layout::Undefined`](crate::Layout::Undefined).
    ///
    /// A placed resource keeps its heap alive. Buffers taking over aliased memory are activated by
    /// a barrier from [`BufferState::NONE`](crate::BufferState::NONE).
    pub fn create_placed_buffer(
        &self,
        heap: &Heap,
        offset: u64,
        desc: &BufferDesc,
    ) -> Result<Buffer> {
        validate_buffer(desc)?;
        ensure(desc.memory == heap.desc.memory)?;
        ensure(matches!(heap.desc.kind, HeapKind::Any | HeapKind::Buffers))?;
        let req = self.buffer_requirements(desc);
        ensure(offset.is_multiple_of(req.alignment))?;
        ensure(
            offset
                .checked_add(req.size)
                .is_some_and(|e| e <= heap.desc.size),
        )?;
        Ok(Buffer {
            raw: Backend::create_placed_buffer(&self.raw, &heap.raw, offset, desc)?,
            desc: *desc,
        })
    }

    /// A texture in `heap` at `offset`; see [`create_placed_buffer`](Self::create_placed_buffer).
    pub fn create_placed_texture(
        &self,
        heap: &Heap,
        offset: u64,
        desc: &TextureDesc,
    ) -> Result<Texture> {
        let req = self.texture_requirements(desc)?;
        ensure(heap.desc.memory == MemoryLocation::GpuOnly)?;
        let target = desc.usage.contains(TextureUsage::RENDER_TARGET)
            || desc.usage.contains(TextureUsage::DEPTH_STENCIL);
        ensure(match heap.desc.kind {
            HeapKind::Any => true,
            HeapKind::Buffers => false,
            HeapKind::Textures => !target,
            HeapKind::Targets => target,
        })?;
        ensure(offset.is_multiple_of(req.alignment))?;
        ensure(
            offset
                .checked_add(req.size)
                .is_some_and(|e| e <= heap.desc.size),
        )?;
        Ok(Texture {
            raw: Backend::create_placed_texture(&self.raw, &heap.raw, offset, desc)?,
            desc: *desc,
        })
    }
}

fn validate_buffer(desc: &BufferDesc) -> Result<()> {
    ensure(desc.size > 0)?;
    ensure(
        !desc.usage.contains(BufferUsage::SHADER_WRITE) || desc.memory == MemoryLocation::GpuOnly,
    )
}

fn validate_texture(d: &TextureDesc) -> Result<()> {
    ensure((1..=MAX_TEXTURE_SIZE).contains(&d.width))?;
    ensure((1..=MAX_TEXTURE_SIZE).contains(&d.height))?;
    ensure((1..=MAX_TEXTURE_LAYERS).contains(&d.array_layers))?;
    match d.dimension {
        TextureDimension::D2 => ensure(d.depth == 1)?,
        TextureDimension::D3 => {
            // D3D12's 3D limits; volumes are sampled or written, not rendered to as targets.
            ensure(d.width <= MAX_TEXTURE_3D_SIZE && d.height <= MAX_TEXTURE_3D_SIZE)?;
            ensure((1..=MAX_TEXTURE_3D_SIZE).contains(&d.depth) && d.array_layers == 1)?;
            ensure(!d.format.is_depth() && !d.usage.contains(TextureUsage::RENDER_TARGET))?;
        }
    }
    let max_mips = 32 - d.width.max(d.height).max(d.depth).leading_zeros();
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
        for queue in [Some(&self.queue), self.compute.as_ref(), self.copy.as_ref()]
            .into_iter()
            .flatten()
        {
            let _ = queue.wait_idle();
        }
    }
}

/// A GPU queue: executes command lists in submission order. Queues run concurrently;
/// [`SubmitDesc::waits`] orders work across them.
pub struct Queue {
    raw: <Backend as Types>::Queue,
    ty: QueueType,
}

impl Queue {
    #[inline]
    pub fn queue_type(&self) -> QueueType {
        self.ty
    }

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
        let signals = signal.as_slice();
        // SAFETY: forwarded from the caller.
        unsafe {
            self.submit_with(&SubmitDesc {
                waits: &[],
                lists,
                signals,
            })
        }
    }

    /// Waits on the GPU for `desc.waits`, executes the lists, then signals `desc.signals`.
    /// `InvalidInput` if a list was recorded for another queue type.
    ///
    /// # Safety
    /// As [`submit`](Self::submit). A wait that is never signalled hangs the queue.
    pub unsafe fn submit_with(&self, desc: &SubmitDesc<'_>) -> Result<()> {
        ensure(desc.lists.iter().all(|l| l.backend_ty == self.ty))?;
        // SAFETY: forwarded from the caller.
        unsafe { Backend::submit(&self.raw, desc) }
    }

    /// Ticks per second of this queue's timestamps ([`CommandList::write_timestamp`]).
    pub fn timestamp_frequency(&self) -> Result<u64> {
        Backend::timestamp_frequency(&self.raw)
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
    /// The work its lists record.
    ty: QueueType,
    /// The queue they run on (graphics where there is no dedicated queue of `ty`).
    backend_ty: QueueType,
}

impl CommandPool {
    #[inline]
    pub fn queue_type(&self) -> QueueType {
        self.ty
    }

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
///
/// A list records the commands of its pool's [`QueueType`]: copy lists copy (and barrier,
/// time and mark), compute lists also dispatch, graphics lists do everything. Recording a
/// command the type does not allow panics.
pub struct CommandList {
    pub(crate) raw: <Backend as Types>::CommandList,
    ty: QueueType,
    backend_ty: QueueType,
}

impl CommandList {
    #[inline]
    pub fn queue_type(&self) -> QueueType {
        self.ty
    }

    fn graphics(&self, what: &str) {
        assert!(
            self.ty == QueueType::Graphics,
            "{what} needs a graphics list, not {:?}",
            self.ty
        );
    }

    fn compute(&self, what: &str) {
        assert!(
            self.ty != QueueType::Copy,
            "{what} needs a graphics or compute list, not a copy list"
        );
    }

    /// Starts recording into `pool` (of the list's queue type), discarding what was recorded
    /// before. The previous recording must not be pending on the GPU.
    pub fn begin(&mut self, pool: &mut CommandPool) -> Result<()> {
        ensure(pool.ty == self.ty)?;
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
        self.graphics("a render pass");
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
        self.graphics("a graphics pipeline");
        Backend::set_graphics_pipeline(&mut self.raw, &pipeline.raw);
    }

    pub fn set_compute_pipeline(&mut self, pipeline: &ComputePipeline) {
        self.compute("a compute pipeline");
        Backend::set_compute_pipeline(&mut self.raw, &pipeline.raw);
    }

    /// Writes `data` at word `offset` of the push constants of the pipeline kind bound last
    /// (graphics if none). Panics past [`MAX_PUSH_CONSTANTS`] words.
    pub fn push_constants(&mut self, offset: u32, data: &[u32]) {
        self.compute("push constants");
        assert!(
            (offset as usize)
                .checked_add(data.len())
                .is_some_and(|end| end <= MAX_PUSH_CONSTANTS as usize)
        );
        Backend::push_constants(&mut self.raw, offset, data);
    }

    pub fn set_viewport(&mut self, viewport: &Viewport) {
        self.graphics("a viewport");
        Backend::set_viewport(&mut self.raw, viewport);
    }

    pub fn set_scissor(&mut self, rect: &Rect) {
        self.graphics("a scissor");
        Backend::set_scissor(&mut self.raw, rect);
    }

    /// The reference value of stencil tests and [`StencilOp::Replace`](crate::StencilOp::Replace)
    /// (0 at the start of each recording).
    pub fn set_stencil_reference(&mut self, reference: u8) {
        self.graphics("a stencil reference");
        Backend::set_stencil_reference(&mut self.raw, reference);
    }

    /// Panics without [`BufferUsage::INDEX`] or with an offset past the end or misaligned.
    pub fn set_index_buffer(&mut self, buffer: &Buffer, offset: u64, format: IndexFormat) {
        self.graphics("an index buffer");
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
        self.graphics("a draw");
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
        self.graphics("a draw");
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
        self.compute("a dispatch");
        Backend::dispatch(&mut self.raw, x, y, z);
    }

    /// Binds a mesh pipeline; push constants then go to it, as for a graphics pipeline.
    pub fn set_mesh_pipeline(&mut self, pipeline: &MeshPipeline) {
        self.graphics("a mesh pipeline");
        Backend::set_mesh_pipeline(&mut self.raw, &pipeline.raw);
    }

    /// Launches `x * y * z` amplification (or, without one, mesh) shader groups.
    pub fn dispatch_mesh(&mut self, x: u32, y: u32, z: u32) {
        self.graphics("a mesh dispatch");
        Backend::dispatch_mesh(&mut self.raw, x, y, z);
    }

    fn check_indirect(args: &IndirectArgs<'_>, stride: u64) {
        let b = &args.buffer.desc;
        assert!(
            b.usage.contains(BufferUsage::INDIRECT),
            "argument buffer without INDIRECT usage"
        );
        assert!(
            args.offset.is_multiple_of(4),
            "argument offset not a multiple of 4"
        );
        assert!(
            (args.max_count as u64)
                .checked_mul(stride)
                .and_then(|n| n.checked_add(args.offset))
                .is_some_and(|e| e <= b.size),
            "indirect arguments past the end of the buffer"
        );
        if let Some((count, offset)) = args.count {
            assert!(
                count.desc.usage.contains(BufferUsage::INDIRECT),
                "count buffer without INDIRECT usage"
            );
            assert!(
                offset.is_multiple_of(4) && offset + 4 <= count.desc.size,
                "count out of range"
            );
        }
    }

    /// Draws as [`draw`](Self::draw) does, once per [`DrawIndirectArgs`] record.
    pub fn draw_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a draw");
        Self::check_indirect(args, size_of::<DrawIndirectArgs>() as u64);
        Backend::draw_indirect(&mut self.raw, args);
    }

    /// Draws as [`draw_indexed`](Self::draw_indexed) does, once per [`DrawIndexedIndirectArgs`]
    /// record.
    pub fn draw_indexed_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a draw");
        Self::check_indirect(args, size_of::<DrawIndexedIndirectArgs>() as u64);
        Backend::draw_indexed_indirect(&mut self.raw, args);
    }

    /// Dispatches once per [`DispatchIndirectArgs`] record.
    pub fn dispatch_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.compute("a dispatch");
        Self::check_indirect(args, size_of::<DispatchIndirectArgs>() as u64);
        Backend::dispatch_indirect(&mut self.raw, args);
    }

    /// Mesh-dispatches once per [`DispatchIndirectArgs`] record.
    pub fn dispatch_mesh_indirect(&mut self, args: &IndirectArgs<'_>) {
        self.graphics("a mesh dispatch");
        Self::check_indirect(args, size_of::<DispatchIndirectArgs>() as u64);
        Backend::dispatch_mesh_indirect(&mut self.raw, args);
    }

    /// Writes the GPU clock into timestamp query `index` once the work before it has finished.
    pub fn write_timestamp(&mut self, pool: &QueryPool, index: u32) {
        let copy = pool.desc.kind == QueryKind::CopyTimestamp;
        assert!(
            copy == (self.ty == QueueType::Copy),
            "copy lists write CopyTimestamp queries, other lists Timestamp queries"
        );
        assert!(index < pool.desc.count, "query index out of range");
        Backend::write_timestamp(&mut self.raw, &pool.raw, index);
    }

    /// Writes queries `first..first + count` as little-endian `u64`s at `offset` (a multiple of
    /// 8) of `dst` ([`BufferUsage::COPY_DST`], in [`Access::COPY_DST`](crate::Access::COPY_DST)).
    pub fn resolve_queries(
        &mut self,
        pool: &QueryPool,
        first: u32,
        count: u32,
        dst: &Buffer,
        offset: u64,
    ) {
        assert!(
            first
                .checked_add(count)
                .is_some_and(|e| e <= pool.desc.count)
                && count > 0,
            "query range out of bounds"
        );
        assert!(dst.desc.usage.contains(BufferUsage::COPY_DST));
        assert!(
            offset.is_multiple_of(8) && offset + count as u64 * 8 <= dst.desc.size,
            "query results out of range"
        );
        Backend::resolve_queries(&mut self.raw, &pool.raw, first, count, dst, offset);
    }

    /// Opens a named region for GPU debuggers and profilers (PIX, RenderDoc). Regions nest;
    /// close each with [`end_event`](Self::end_event) in the same list.
    pub fn begin_event(&mut self, name: &str) {
        Backend::begin_event(&mut self.raw, name);
    }

    pub fn end_event(&mut self) {
        Backend::end_event(&mut self.raw);
    }

    /// A named point in the GPU timeline, for debuggers.
    pub fn set_marker(&mut self, name: &str) {
        Backend::set_marker(&mut self.raw, name);
    }

    /// Copies a box between textures of the same format (or its sRGB / linear twin). Panics if
    /// either side is out of range, misaligned to blocks, or lacks its copy usage. Depth-stencil
    /// copies move both planes.
    pub fn copy_texture(&mut self, src: &Texture, dst: &Texture, region: &TextureCopy) {
        assert!(src.desc.usage.contains(TextureUsage::COPY_SRC));
        assert!(dst.desc.usage.contains(TextureUsage::COPY_DST));
        let (sf, df) = (src.desc.format, dst.desc.format);
        assert!(
            sf.linear() == df.linear(),
            "copy between incompatible formats"
        );
        let fits = |t: &TextureDesc, mip: u32, layer: u32, offset: [u32; 3]| {
            let (w, h) = t.mip_size(mip);
            let block = t.format.block_extent();
            let (w, h) = (w.next_multiple_of(block), h.next_multiple_of(block));
            let d = t.mip_depth(mip);
            mip < t.mip_levels
                && layer < t.array_layers
                && [w, h, d].iter().zip(offset.iter().zip(&region.size)).all(
                    |(&extent, (&o, &s))| s > 0 && o.checked_add(s).is_some_and(|e| e <= extent),
                )
                && [offset[0], offset[1], region.size[0], region.size[1]]
                    .iter()
                    .all(|v| v.is_multiple_of(block))
        };
        assert!(
            fits(
                &src.desc,
                region.src_mip,
                region.src_layer,
                region.src_offset
            ),
            "copy source out of range"
        );
        assert!(
            fits(
                &dst.desc,
                region.dst_mip,
                region.dst_layer,
                region.dst_offset
            ),
            "copy destination out of range"
        );
        if sf.is_depth() {
            assert!(
                region.src_offset == [0; 3]
                    && region.dst_offset == [0; 3]
                    && (region.size[0], region.size[1]) == src.desc.mip_size(region.src_mip),
                "depth copies cover the whole subresource"
            );
        }
        Backend::copy_texture(&mut self.raw, src, dst, region);
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
            && r.z
                .checked_add(r.depth)
                .is_some_and(|e| e <= d.mip_depth(r.mip))
            && r.width > 0
            && r.height > 0
            && r.depth > 0,
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
    let row = (r.width / block) as u64 * d.format.copy_block_size() as u64;
    assert!(r.bytes_per_row as u64 >= row, "copy rows overlap");
    // Slices follow each other, `height / block` rows apart.
    let rows = (r.height / block) as u64 * r.depth as u64;
    let end = r.buffer_offset + (rows - 1) * r.bytes_per_row as u64 + row;
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

/// Compiled amplification, mesh and pixel shaders plus fixed-function state, for mesh dispatches.
pub struct MeshPipeline {
    raw: <Backend as Types>::MeshPipeline,
}

/// GPU queries of one kind (timestamps).
pub struct QueryPool {
    raw: <Backend as Types>::QueryPool,
    desc: QueryPoolDesc,
}

impl QueryPool {
    #[inline]
    pub fn desc(&self) -> &QueryPoolDesc {
        &self.desc
    }
}

/// GPU memory that placed resources are created in.
pub struct Heap {
    raw: <Backend as Types>::Heap,
    desc: HeapDesc,
}

impl Heap {
    #[inline]
    pub fn desc(&self) -> &HeapDesc {
        &self.desc
    }
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
    MeshPipeline,
    Swapchain
);

impl core::fmt::Debug for QueryPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("QueryPool")
            .field("desc", &self.desc)
            .finish()
    }
}

impl core::fmt::Debug for Heap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Heap").field("desc", &self.desc).finish()
    }
}

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
