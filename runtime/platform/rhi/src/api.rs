//! Public types: thin wrappers forwarding to the selected backend.
//!
//! Descriptions are validated here, once for every backend: invalid ones return `InvalidInput`
//! and missing optional features `Unsupported`; out-of-range recording arguments (copies, push
//! constants) panic, as slice indexing does. Recording lives in `commands.rs`.

use core::ffi::c_void;
use core::ptr::NonNull;
use core::time::Duration;

use alloc::vec::Vec;

use crate::backend::imp::Backend;
use crate::contract::{
    Device as _, Fences, Handles, Instance as _, Object, Queue as _, Resources, Swapchain as _,
    Types,
};
use crate::{
    AccelerationStructureGeometry, AccelerationStructureInputs, AccelerationStructureKind,
    AccelerationStructureSizes, AdapterInfo, BufferDesc, BufferUsage, BufferViewDesc,
    BufferViewKind, BuildFlags, ClearValue, ClockCalibration, ColorSpace, ColorTargetState,
    ComputePipelineDesc, DepthState, DepthStencilViewDesc, DeviceDesc, Error, ErrorKind, Features,
    FenceWait, Format, FormatFeatures, FormatSupport, FrameStatistics, GraphicsPipelineDesc,
    HdrMetadata, HeapDesc, HeapKind, InstanceDesc, MAX_COLOR_ATTACHMENTS, MAX_UNIFORM_SIZE,
    MemoryBudget, MemoryLocation, MemoryRequirements, MeshPipelineDesc, MultisampleState,
    OutputInfo, Pageable, PresentMode, PresentStatus, PrimitiveTopology, QueryKind, QueryPoolDesc,
    QueueType, RasterState, RenderTargetViewDesc, ResidencyPriority, Result, SPARSE_TILE_SIZE,
    SamplerDesc, SamplerReduction, SparseBind, SparseTextureLayout, SparseTextureRegion,
    SubmitDesc, SubresourceRange, SurfaceHandle, SwapchainDesc, TextureDesc, TextureDimension,
    TextureUsage, TextureViewDesc, TextureViewDimension, UNIFORM_ALIGNMENT, VariableRateShading,
};

pub(crate) fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

pub(crate) fn unsupported() -> Error {
    Error::from_kind(ErrorKind::Unsupported)
}

pub(crate) fn ensure(condition: bool) -> Result<()> {
    if condition { Ok(()) } else { Err(invalid()) }
}

fn require(feature: bool) -> Result<()> {
    if feature { Ok(()) } else { Err(unsupported()) }
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
        ensure(desc.validation || !desc.gpu_validation)?;
        Ok(Self {
            raw: Backend::create_instance(desc)?,
        })
    }

    /// Every adapter, highest performance first; software adapters last.
    pub fn adapters(&self) -> impl Iterator<Item = AdapterInfo> + '_ {
        (0..).map_while(|i| Backend::adapter(&self.raw, i))
    }

    /// The displays attached to adapter `adapter` ([`AdapterInfo::index`]).
    pub fn outputs(&self, adapter: u32) -> impl Iterator<Item = OutputInfo> + '_ {
        (0..).map_while(move |i| Backend::output(&self.raw, adapter, i))
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

    /// The native object (D3D12: the `IDXGIFactory4*`), borrowed: valid while `self` lives.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Instance(&self.raw))
    }
}

/// A GPU. Owns its queues and the bindless tables. Dropping it waits for the queues to go idle.
///
/// # Losing the device
///
/// A GPU hang, crash, driver update or removal loses the device: calls start returning
/// [`ErrorKind::DeviceLost`], and with [`InstanceDesc::crash_diagnostics`] the first of them logs
/// what the GPU was doing. Every object of the device is then dead. Recover by dropping all of
/// them (the device last), then creating a new device from the [`Instance`] and recreating the
/// resources; [`status`](Self::status) checks without doing anything else.
pub struct Device {
    queue: Queue,
    compute: Option<Queue>,
    copy: Option<Queue>,
    pub(crate) raw: <Backend as Types>::Device,
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

    /// What the device can do with `format`.
    pub fn format_support(&self, format: Format) -> FormatSupport {
        Backend::format_support(&self.raw, format)
    }

    /// `Err(DeviceLost)` once the device is gone (see [`Device`]).
    pub fn status(&self) -> Result<()> {
        Backend::status(&self.raw)
    }

    /// Loses the device on purpose, to test the recovery path. Every later call reports
    /// `DeviceLost`, here and (D3D12 shares one device per adapter in a process) in any other
    /// `Device` of the same adapter.
    pub fn force_device_lost(&self) {
        Backend::force_device_lost(&self.raw);
    }

    /// Pins the GPU clocks so profiling runs compare (D3D12 needs Windows developer mode). Never
    /// in shipping builds: it costs performance and power.
    pub fn set_stable_power_state(&self, stable: bool) -> Result<()> {
        Backend::set_stable_power_state(&self.raw, stable)
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
            raw: Backend::create_fence(&self.raw, initial_value, false)?,
        })
    }

    /// A fence that [`export_fence`](Self::export_fence) can hand to another device or process.
    pub fn create_shared_fence(&self, initial_value: u64) -> Result<Fence> {
        Ok(Fence {
            raw: Backend::create_fence(&self.raw, initial_value, true)?,
        })
    }

    /// Blocks until every fence (or, with [`FenceWait::Any`], one of them) reaches its value.
    /// `Ok(false)` if `timeout` passed first.
    pub fn wait_for_fences(
        &self,
        fences: &[(&Fence, u64)],
        mode: FenceWait,
        timeout: Option<Duration>,
    ) -> Result<bool> {
        ensure(!fences.is_empty())?;
        Backend::wait_for_fences(&self.raw, fences, mode, timeout)
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
            features: self.features(),
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
        validate_color_space(desc.format, desc.color_space)?;
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
        self.validate_buffer(desc)?;
        Ok(Buffer {
            raw: Backend::create_buffer(&self.raw, desc)?,
            desc: *desc,
            kind: ResourceKind::Committed,
        })
    }

    pub fn create_texture(&self, desc: &TextureDesc) -> Result<Texture> {
        self.validate_texture(desc)?;
        Ok(Texture {
            raw: Backend::create_texture(&self.raw, desc)?,
            desc: *desc,
            kind: ResourceKind::Committed,
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
                TextureViewDimension::CubeArray => (t.array_layers - r.base_layer) / 6 * 6,
            },
            n => n,
        };
        ensure(mip_count > 0 && r.base_mip + mip_count <= t.mip_levels)?;
        ensure(layer_count > 0 && r.base_layer + layer_count <= t.array_layers)?;
        ensure(!desc.writable || mip_count == 1)?;
        let volume = t.dimension == TextureDimension::D3;
        let multisampled = t.sample_count > 1;
        let cube = !volume && !desc.writable && !multisampled && t.width == t.height;
        match desc.dimension {
            TextureViewDimension::D2 => ensure(layer_count == 1 && r.base_layer == 0 && !volume)?,
            TextureViewDimension::D2Array => ensure(!volume)?,
            TextureViewDimension::Cube => ensure(cube && layer_count == 6)?,
            TextureViewDimension::CubeArray => {
                ensure(cube && layer_count.is_multiple_of(6))?;
            }
            TextureViewDimension::D3 => ensure(volume && layer_count == 1)?,
        }
        let format = match desc.format {
            None => match t.format {
                Format::D16Unorm => Format::R16Unorm,
                Format::D32Float | Format::D32FloatS8 => Format::R32Float,
                f => f,
            },
            Some(f) => {
                let plane = matches!(
                    (t.format, f),
                    (Format::D16Unorm, Format::R16Unorm)
                        | (Format::D32Float | Format::D32FloatS8, Format::R32Float)
                        | (Format::D32FloatS8, Format::R8Uint)
                );
                ensure(plane || f.linear() == t.format.linear())?;
                f
            }
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
        if desc.reduction != SamplerReduction::WeightedAverage {
            require(self.features().sampler_min_max)?;
            ensure(desc.compare.is_none())?;
        }
        Ok(Sampler {
            raw: Backend::create_sampler(&self.raw, desc)?,
        })
    }

    fn validate_raster(
        &self,
        raster: &RasterState,
        depth: &Option<DepthState>,
        color_targets: &[ColorTargetState],
        multisample: &MultisampleState,
    ) -> Result<()> {
        let features = self.features();
        ensure(color_targets.len() <= MAX_COLOR_ATTACHMENTS)?;
        ensure(depth.is_none_or(|d| {
            d.format.is_depth() && (d.stencil.is_none() || d.format.has_stencil())
        }))?;
        if depth.is_some_and(|d| d.bounds_test) {
            require(features.depth_bounds)?;
        }
        ensure(
            color_targets
                .iter()
                .all(|t| !t.format.is_depth() && !t.format.is_compressed()),
        )?;
        let dual_source = color_targets.iter().any(|t| {
            t.blend.is_some_and(|b| {
                [b.color, b.alpha]
                    .iter()
                    .any(|c| c.src_factor.is_dual_source() || c.dst_factor.is_dual_source())
            })
        });
        ensure(!dual_source || color_targets.len() == 1)?;
        ensure(
            color_targets
                .iter()
                .all(|t| t.blend.is_none() || !t.format.is_integer()),
        )?;
        if raster.conservative {
            require(features.conservative_rasterization)?;
        }
        ensure(matches!(multisample.count, 1 | 2 | 4 | 8 | 16))?;
        let formats = color_targets
            .iter()
            .map(|t| t.format)
            .chain(depth.map(|d| d.format));
        for format in formats {
            if !self
                .format_support(format)
                .supports_samples(multisample.count)
            {
                return Err(unsupported());
            }
        }
        Ok(())
    }

    pub fn create_graphics_pipeline(
        &self,
        desc: &GraphicsPipelineDesc<'_>,
    ) -> Result<GraphicsPipeline> {
        self.validate_raster(
            &desc.raster,
            &desc.depth,
            desc.color_targets,
            &desc.multisample,
        )?;
        ensure(!desc.vertex.code.is_empty() && desc.pixel.is_none_or(|p| !p.code.is_empty()))?;
        ensure(
            desc.primitive_restart.is_none()
                || matches!(
                    desc.topology,
                    PrimitiveTopology::LineStrip | PrimitiveTopology::TriangleStrip
                ),
        )?;
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
        require(self.features().mesh_shader)?;
        self.validate_raster(
            &desc.raster,
            &desc.depth,
            desc.color_targets,
            &desc.multisample,
        )?;
        ensure(!desc.mesh.code.is_empty())?;
        ensure(desc.amplification.is_none_or(|a| !a.code.is_empty()))?;
        ensure(desc.pixel.is_none_or(|p| !p.code.is_empty()))?;
        Ok(MeshPipeline {
            raw: Backend::create_mesh_pipeline(&self.raw, desc)?,
        })
    }

    /// A cache of compiled pipelines, seeded with what an earlier
    /// [`PipelineCache::serialize`] returned (empty for a fresh one). Data from another driver
    /// version or adapter is ignored: the cache starts empty and pipelines compile as usual.
    pub fn create_pipeline_cache(&self, data: &[u8]) -> Result<PipelineCache> {
        Ok(PipelineCache {
            raw: Backend::create_pipeline_cache(&self.raw, data)?,
        })
    }

    /// `count` queries (1..=65536) of one kind.
    pub fn create_query_pool(&self, desc: &QueryPoolDesc) -> Result<QueryPool> {
        ensure((1..=65536).contains(&desc.count))?;
        let features = self.features();
        match desc.kind {
            QueryKind::CopyTimestamp => require(features.copy_queue_timestamps)?,
            QueryKind::PipelineStatistics => require(features.pipeline_statistics)?,
            _ => {}
        }
        Ok(QueryPool {
            raw: Backend::create_query_pool(&self.raw, desc)?,
            desc: *desc,
        })
    }

    /// Memory for placed resources ([`create_placed_buffer`](Self::create_placed_buffer),
    /// [`create_placed_texture`](Self::create_placed_texture)) and for the tiles of sparse ones:
    /// pools of streamed textures and the render graph's transient resources, which alias each
    /// other.
    pub fn create_heap(&self, desc: &HeapDesc) -> Result<Heap> {
        ensure(desc.size > 0)?;
        ensure(desc.kind != HeapKind::Any || self.features().mixed_heaps)?;
        ensure(
            desc.memory == MemoryLocation::GpuOnly
                || matches!(desc.kind, HeapKind::Any | HeapKind::Buffers),
        )?;
        if desc.memory == MemoryLocation::GpuUpload {
            require(self.features().gpu_upload_heap)?;
        }
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
        self.validate_texture(desc)?;
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
        self.validate_buffer(desc)?;
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
            kind: ResourceKind::Placed,
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
        ensure(heap_takes_texture(heap.desc.kind, desc))?;
        ensure(offset.is_multiple_of(req.alignment))?;
        ensure(
            offset
                .checked_add(req.size)
                .is_some_and(|e| e <= heap.desc.size),
        )?;
        Ok(Texture {
            raw: Backend::create_placed_texture(&self.raw, &heap.raw, offset, desc)?,
            desc: *desc,
            kind: ResourceKind::Placed,
        })
    }

    // --- Memory budget and residency ---

    /// How much memory the OS lets this process use now, and how much it uses. Poll it (once a
    /// frame is cheap) and shed streaming memory when [`MemoryUsage::headroom`] runs out: over
    /// budget, the OS pages and frames stutter.
    ///
    /// [`MemoryUsage::headroom`]: crate::MemoryUsage::headroom
    pub fn memory_budget(&self) -> Result<MemoryBudget> {
        Backend::memory_budget(&self.raw)
    }

    /// Asks the OS to keep this much local and non-local memory for the process even under
    /// pressure (at most [`MemoryUsage::available_for_reservation`](crate::MemoryUsage)).
    pub fn set_memory_reservation(&self, local: u64, non_local: u64) -> Result<()> {
        Backend::set_memory_reservation(&self.raw, local, non_local)
    }

    /// Brings evicted objects back into GPU memory; blocks until they are. Objects are resident
    /// when created.
    pub fn make_resident(&self, objects: &[Pageable<'_>]) -> Result<()> {
        validate_pageables(objects)?;
        Backend::make_resident(&self.raw, objects)
    }

    /// Lets the OS page objects out (unused streaming pools, inactive levels). The GPU must not
    /// use them until [`make_resident`](Self::make_resident).
    pub fn evict(&self, objects: &[Pageable<'_>]) -> Result<()> {
        validate_pageables(objects)?;
        Backend::evict(&self.raw, objects)
    }

    /// Which objects the OS pages out first when the process goes over budget.
    pub fn set_residency_priority(
        &self,
        objects: &[Pageable<'_>],
        priority: ResidencyPriority,
    ) -> Result<()> {
        validate_pageables(objects)?;
        Backend::set_residency_priority(&self.raw, objects, priority)
    }

    // --- Acceleration structures ---

    /// The memory a structure built from `inputs` with `flags` needs. Requires
    /// [`Features::ray_query`].
    pub fn acceleration_structure_sizes(
        &self,
        inputs: &AccelerationStructureInputs<'_>,
        flags: BuildFlags,
    ) -> Result<AccelerationStructureSizes> {
        require(self.features().ray_query)?;
        validate_inputs(inputs)?;
        Ok(Backend::acceleration_structure_sizes(
            &self.raw, inputs, flags,
        ))
    }

    /// An acceleration structure of `size` bytes (from
    /// [`acceleration_structure_sizes`](Self::acceleration_structure_sizes)), unbuilt, in a
    /// committed storage buffer of its own. Its [`index`](AccelerationStructure::index) is a
    /// bindless slot shaders open as a `RaytracingAccelerationStructure` for ray queries
    /// (top-level structures).
    pub fn create_acceleration_structure(
        &self,
        kind: AccelerationStructureKind,
        size: u64,
    ) -> Result<AccelerationStructure> {
        require(self.features().ray_query)?;
        ensure(size > 0)?;
        let storage = self.create_buffer(&BufferDesc {
            size: size.next_multiple_of(crate::ACCELERATION_STRUCTURE_ALIGNMENT),
            usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE,
            memory: MemoryLocation::GpuOnly,
        })?;
        self.create_acceleration_structure_in(&storage, 0, kind, size)
    }

    /// An acceleration structure of `size` bytes at `offset` (a multiple of
    /// [`ACCELERATION_STRUCTURE_ALIGNMENT`](crate::ACCELERATION_STRUCTURE_ALIGNMENT)) of
    /// `storage`, a buffer with [`BufferUsage::ACCELERATION_STRUCTURE_STORAGE`]: many small
    /// structures sub-allocated from one committed or placed buffer. It keeps the buffer alive;
    /// structures in one buffer must not overlap while in use, and share its barriers (one
    /// [`buffer_barriers`](crate::CommandList::buffer_barriers) entry, or a
    /// [`memory_barrier`](crate::CommandList::memory_barrier), covers all of them).
    pub fn create_acceleration_structure_in(
        &self,
        storage: &Buffer,
        offset: u64,
        kind: AccelerationStructureKind,
        size: u64,
    ) -> Result<AccelerationStructure> {
        require(self.features().ray_query)?;
        ensure(
            storage
                .desc
                .usage
                .contains(BufferUsage::ACCELERATION_STRUCTURE_STORAGE),
        )?;
        let size = size.next_multiple_of(crate::ACCELERATION_STRUCTURE_ALIGNMENT);
        ensure(size > 0 && offset.is_multiple_of(crate::ACCELERATION_STRUCTURE_ALIGNMENT))?;
        ensure(
            offset
                .checked_add(size)
                .is_some_and(|e| e <= storage.desc.size),
        )?;
        let (shared, raw) =
            Backend::create_acceleration_structure(&self.raw, &storage.raw, offset, kind)?;
        Ok(AccelerationStructure {
            raw,
            storage: Buffer {
                raw: shared,
                desc: storage.desc,
                kind: storage.kind,
            },
            offset,
            size,
            kind,
        })
    }

    // --- Sparse resources ---

    /// A buffer with address space but no memory: bind [`SPARSE_TILE_SIZE`] tiles of heap memory
    /// to it with [`Queue::bind_sparse`]. Unbound tiles read zero and drop writes. Requires
    /// [`Features::sparse_resources`]; [`MemoryLocation::GpuOnly`].
    pub fn create_sparse_buffer(&self, desc: &BufferDesc) -> Result<Buffer> {
        require(self.features().sparse_resources)?;
        self.validate_buffer(desc)?;
        ensure(
            desc.memory == MemoryLocation::GpuOnly
                && !desc
                    .usage
                    .contains(BufferUsage::ACCELERATION_STRUCTURE_STORAGE),
        )?;
        Ok(Buffer {
            raw: Backend::create_sparse_buffer(&self.raw, desc)?,
            desc: *desc,
            kind: ResourceKind::Sparse,
        })
    }

    /// A texture with no memory, bound tile by tile ([`sparse_texture_layout`]): virtual
    /// texturing, virtual shadow maps. Requires [`Features::sparse_resources`] (and
    /// [`Features::sparse_volumes`] for volumes) and a format with [`FormatFeatures::SPARSE`];
    /// single-sampled, no depth.
    ///
    /// [`sparse_texture_layout`]: Self::sparse_texture_layout
    pub fn create_sparse_texture(&self, desc: &TextureDesc) -> Result<Texture> {
        let features = self.features();
        require(features.sparse_resources)?;
        if desc.dimension == TextureDimension::D3 {
            require(features.sparse_volumes)?;
        }
        self.validate_texture(desc)?;
        ensure(desc.sample_count == 1 && !desc.format.is_depth())?;
        require(
            self.format_support(desc.format)
                .features
                .contains(FormatFeatures::SPARSE),
        )?;
        Ok(Texture {
            raw: Backend::create_sparse_texture(&self.raw, desc)?,
            desc: *desc,
            kind: ResourceKind::Sparse,
        })
    }

    /// How a sparse texture's mips divide into tiles. Panics for other textures.
    pub fn sparse_texture_layout(&self, texture: &Texture) -> SparseTextureLayout {
        assert!(texture.kind == ResourceKind::Sparse, "not a sparse texture");
        Backend::sparse_texture_layout(&self.raw, texture)
    }

    // --- Sharing and native objects ---

    /// An OS handle another device, API or process can open the buffer with. Requires
    /// [`BufferUsage::SHARED`].
    pub fn export_buffer(&self, buffer: &Buffer) -> Result<SharedHandle> {
        ensure(buffer.desc.usage.contains(BufferUsage::SHARED))?;
        Ok(SharedHandle {
            raw: Backend::export(&self.raw, Object::Buffer(&buffer.raw))?,
        })
    }

    /// As [`export_buffer`](Self::export_buffer), for a texture with [`TextureUsage::SHARED`].
    pub fn export_texture(&self, texture: &Texture) -> Result<SharedHandle> {
        ensure(texture.desc.usage.contains(TextureUsage::SHARED))?;
        Ok(SharedHandle {
            raw: Backend::export(&self.raw, Object::Texture(&texture.raw))?,
        })
    }

    /// As [`export_buffer`](Self::export_buffer), for a fence made by
    /// [`create_shared_fence`](Self::create_shared_fence).
    pub fn export_fence(&self, fence: &Fence) -> Result<SharedHandle> {
        Ok(SharedHandle {
            raw: Backend::export(&self.raw, Object::Fence(&fence.raw))?,
        })
    }

    /// Opens a texture another device or process exported.
    ///
    /// # Safety
    /// `handle` names a texture that `desc` describes exactly.
    pub unsafe fn import_texture(
        &self,
        handle: &SharedHandle,
        desc: &TextureDesc,
    ) -> Result<Texture> {
        self.validate_texture(desc)?;
        Ok(Texture {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::import_texture(&self.raw, &handle.raw, desc)? },
            desc: *desc,
            kind: ResourceKind::External,
        })
    }

    /// Opens a buffer another device or process exported.
    ///
    /// # Safety
    /// `handle` names a buffer that `desc` describes exactly.
    pub unsafe fn import_buffer(&self, handle: &SharedHandle, desc: &BufferDesc) -> Result<Buffer> {
        self.validate_buffer(desc)?;
        Ok(Buffer {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::import_buffer(&self.raw, &handle.raw, desc)? },
            desc: *desc,
            kind: ResourceKind::External,
        })
    }

    /// Opens a fence another device or process exported.
    ///
    /// # Safety
    /// `handle` names a fence.
    pub unsafe fn import_fence(&self, handle: &SharedHandle) -> Result<Fence> {
        Ok(Fence {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::import_fence(&self.raw, &handle.raw)? },
        })
    }

    /// Wraps a texture another library created on this device (an XR runtime's swapchain
    /// image, an upscaler's output). The wrapper adds its own reference to it.
    ///
    /// # Safety
    /// `raw` is a live native texture (D3D12: `ID3D12Resource*`) of this device, in the common
    /// state, and `desc` describes it exactly.
    pub unsafe fn texture_from_raw(&self, raw: *mut c_void, desc: &TextureDesc) -> Result<Texture> {
        self.validate_texture(desc)?;
        ensure(!raw.is_null())?;
        Ok(Texture {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::texture_from_native(&self.raw, raw, desc)? },
            desc: *desc,
            kind: ResourceKind::External,
        })
    }

    /// As [`texture_from_raw`](Self::texture_from_raw), for a buffer.
    ///
    /// # Safety
    /// `raw` is a live native buffer of this device that `desc` describes exactly; mapped
    /// memory locations must match its heap.
    pub unsafe fn buffer_from_raw(&self, raw: *mut c_void, desc: &BufferDesc) -> Result<Buffer> {
        self.validate_buffer(desc)?;
        ensure(!raw.is_null())?;
        Ok(Buffer {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::buffer_from_native(&self.raw, raw, desc)? },
            desc: *desc,
            kind: ResourceKind::External,
        })
    }

    /// The native device (D3D12: `ID3D12Device*`), borrowed, for libraries that take one
    /// (upscalers, XR runtimes, vendor SDKs).
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Device(&self.raw))
    }

    /// Names the device for debuggers.
    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::Device(&self.raw), name);
    }

    fn validate_buffer(&self, desc: &BufferDesc) -> Result<()> {
        ensure(desc.size > 0)?;
        let shader_write = desc.usage.contains(BufferUsage::SHADER_WRITE);
        ensure(!shader_write || desc.memory == MemoryLocation::GpuOnly)?;
        if desc
            .usage
            .contains(BufferUsage::ACCELERATION_STRUCTURE_STORAGE)
        {
            require(self.features().ray_query)?;
            ensure(
                desc.usage == BufferUsage::ACCELERATION_STRUCTURE_STORAGE
                    && desc.memory == MemoryLocation::GpuOnly,
            )?;
        }
        if desc.memory == MemoryLocation::GpuUpload {
            require(self.features().gpu_upload_heap)?;
        }
        Ok(())
    }

    fn validate_texture(&self, d: &TextureDesc) -> Result<()> {
        validate_texture(d)?;
        let support = self.format_support(d.format);
        if support.features.is_empty() {
            return Err(unsupported());
        }
        if d.sample_count > 1 && !support.supports_samples(d.sample_count) {
            return Err(unsupported());
        }
        if d.usage.contains(TextureUsage::SHADING_RATE) {
            require(self.features().variable_rate_shading == VariableRateShading::Image)?;
        }
        Ok(())
    }
}

fn heap_takes_texture(kind: HeapKind, desc: &TextureDesc) -> bool {
    let target = desc.usage.contains(TextureUsage::RENDER_TARGET)
        || desc.usage.contains(TextureUsage::DEPTH_STENCIL);
    match kind {
        HeapKind::Any => true,
        HeapKind::Buffers => false,
        HeapKind::Textures => !target,
        HeapKind::Targets => target,
    }
}

fn validate_color_space(format: Format, color_space: ColorSpace) -> Result<()> {
    ensure(match color_space {
        ColorSpace::Srgb => true,
        ColorSpace::Hdr10 => format == Format::Rgb10a2Unorm,
        ColorSpace::ScRgb => format == Format::Rgba16Float,
    })
}

fn validate_pageables(objects: &[Pageable<'_>]) -> Result<()> {
    ensure(objects.iter().all(|o| match o {
        Pageable::Buffer(b) => b.kind == ResourceKind::Committed,
        Pageable::Texture(t) => t.kind == ResourceKind::Committed,
        Pageable::AccelerationStructure(a) => a.storage.kind == ResourceKind::Committed,
        Pageable::Heap(_) => true,
    }))
}

pub(crate) fn validate_inputs(inputs: &AccelerationStructureInputs<'_>) -> Result<()> {
    match inputs {
        AccelerationStructureInputs::BottomLevel(geometries) => {
            ensure(!geometries.is_empty())?;
            let triangles = matches!(geometries[0], AccelerationStructureGeometry::Triangles(_));
            ensure(
                geometries
                    .iter()
                    .all(|g| matches!(g, AccelerationStructureGeometry::Triangles(_)) == triangles),
            )
        }
        AccelerationStructureInputs::TopLevel { .. } => Ok(()),
    }
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
        ensure(!d.usage.intersects(attachment_or_storage))?;
        let block = f.block_extent();
        ensure(d.width.is_multiple_of(block) && d.height.is_multiple_of(block))?;
    }
    if f.is_srgb() {
        ensure(!d.usage.contains(TextureUsage::STORAGE))?;
    }
    match d.sample_count {
        1 => {}
        2 | 4 | 8 | 16 => {
            ensure(d.dimension == TextureDimension::D2 && d.mip_levels == 1)?;
            ensure(!d.usage.contains(TextureUsage::STORAGE))?;
            ensure(
                d.usage
                    .intersects(TextureUsage::RENDER_TARGET | TextureUsage::DEPTH_STENCIL),
            )?;
        }
        _ => return Err(invalid()),
    }
    if d.usage.contains(TextureUsage::SHADING_RATE) {
        ensure(f == Format::R8Uint && d.dimension == TextureDimension::D2)?;
        ensure(d.sample_count == 1 && d.mip_levels == 1 && d.array_layers == 1)?;
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

    /// The queue's GPU clock and the CPU clock, read together, so profilers can put GPU
    /// timestamps on the CPU timeline. Clocks drift: calibrate again every few seconds.
    pub fn clock_calibration(&self) -> Result<ClockCalibration> {
        Backend::clock_calibration(&self.raw)
    }

    /// Blocks until everything submitted so far has finished.
    pub fn wait_idle(&self) -> Result<()> {
        Backend::wait_idle(&self.raw)
    }

    /// Binds (or unbinds) heap memory to tiles of sparse buffers and textures, in queue order
    /// with this queue's submissions. Heaps must hold the tiles' kind of resource
    /// ([`HeapKind`]); tiles bound twice alias the same memory. Panics if a region lies outside
    /// its resource or heap.
    ///
    /// # Safety
    /// Each heap outlives the bindings into it, and no submitted work uses a tile while it is
    /// rebound.
    pub unsafe fn bind_sparse(&self, binds: &[SparseBind<'_>]) -> Result<()> {
        for bind in binds {
            let (memory, kind) = match bind {
                SparseBind::Buffer {
                    buffer,
                    tile_offset,
                    tile_count,
                    memory,
                } => {
                    assert!(buffer.kind == ResourceKind::Sparse, "not a sparse buffer");
                    let tiles = buffer.desc.size.div_ceil(SPARSE_TILE_SIZE);
                    assert!(
                        *tile_count > 0 && (*tile_offset as u64 + *tile_count as u64) <= tiles,
                        "sparse buffer tiles out of range"
                    );
                    (memory.map(|(h, t)| (h, t, *tile_count)), HeapKind::Buffers)
                }
                SparseBind::Texture {
                    texture,
                    region,
                    memory,
                } => {
                    assert!(texture.kind == ResourceKind::Sparse, "not a sparse texture");
                    let d = &texture.desc;
                    let tiles = match region {
                        SparseTextureRegion::Tiles {
                            mip, layer, size, ..
                        } => {
                            assert!(*mip < d.mip_levels && *layer < d.array_layers);
                            assert!(size.iter().all(|&s| s > 0), "empty sparse region");
                            size[0] * size[1] * size[2]
                        }
                        SparseTextureRegion::PackedMips { layer } => {
                            assert!(*layer < d.array_layers);
                            0
                        }
                    };
                    (memory.map(|(h, t)| (h, t, tiles)), {
                        let target = d.usage.contains(TextureUsage::RENDER_TARGET);
                        if target {
                            HeapKind::Targets
                        } else {
                            HeapKind::Textures
                        }
                    })
                }
            };
            if let Some((heap, first, count)) = memory {
                assert!(
                    heap.desc.memory == MemoryLocation::GpuOnly
                        && (heap.desc.kind == HeapKind::Any || heap.desc.kind == kind),
                    "heap cannot hold these tiles"
                );
                let heap_tiles = heap.desc.size / SPARSE_TILE_SIZE;
                assert!(
                    first as u64 + count as u64 <= heap_tiles,
                    "heap tiles out of range"
                );
            }
        }
        // SAFETY: forwarded from the caller; the ranges were checked above.
        unsafe { Backend::bind_sparse(&self.raw, binds) }
    }

    /// The native queue (D3D12: `ID3D12CommandQueue*`), borrowed.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Queue(&self.raw))
    }

    /// Names the queue for debuggers and crash reports.
    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::Queue(&self.raw), name);
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

    /// Blocks until the fence reaches `value`. Returns `false` if `timeout` passed first, and
    /// `DeviceLost` if the device is gone (its fences never advance again).
    pub fn wait(&self, value: u64, timeout: Option<Duration>) -> Result<bool> {
        Backend::wait(&self.raw, value, timeout)
    }

    /// Sets the value from the CPU.
    pub fn signal(&self, value: u64) -> Result<()> {
        Backend::signal(&self.raw, value)
    }

    /// The native fence (D3D12: `ID3D12Fence*`), borrowed.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Fence(&self.raw))
    }

    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::Fence(&self.raw), name);
    }
}

/// How a buffer's or texture's memory came to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResourceKind {
    Committed,
    Placed,
    Sparse,
    /// Imported or wrapped from a native object.
    External,
    /// A swapchain image.
    Swapchain,
}

/// A linear GPU buffer.
pub struct Buffer {
    pub(crate) raw: <Backend as Types>::Buffer,
    pub(crate) desc: BufferDesc,
    pub(crate) kind: ResourceKind,
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

    /// The CPU address of an [`Upload`](MemoryLocation::Upload),
    /// [`Readback`](MemoryLocation::Readback) or [`GpuUpload`](MemoryLocation::GpuUpload)
    /// buffer, valid for the buffer's life.
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

    /// Created by [`Device::create_sparse_buffer`].
    pub fn is_sparse(&self) -> bool {
        self.kind == ResourceKind::Sparse
    }

    /// The native resource (D3D12: `ID3D12Resource*`), borrowed.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Buffer(&self.raw))
    }

    /// Names the buffer for debuggers and crash reports (page faults name the resource).
    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::Buffer(&self.raw), name);
    }
}

/// A GPU texture.
pub struct Texture {
    pub(crate) raw: <Backend as Types>::Texture,
    pub(crate) desc: TextureDesc,
    pub(crate) kind: ResourceKind,
}

impl Texture {
    #[cfg_attr(
        any(marlin_platform = "linux", marlin_platform = "macos"),
        expect(dead_code, reason = "the stub backend has no swapchains")
    )]
    pub(crate) fn swapchain_image(raw: <Backend as Types>::Texture, desc: TextureDesc) -> Self {
        Self {
            raw,
            desc,
            kind: ResourceKind::Swapchain,
        }
    }

    #[inline]
    pub fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    /// Created by [`Device::create_sparse_texture`].
    pub fn is_sparse(&self) -> bool {
        self.kind == ResourceKind::Sparse
    }

    /// The native resource (D3D12: `ID3D12Resource*`), borrowed.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Texture(&self.raw))
    }

    /// Names the texture for debuggers and crash reports.
    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::Texture(&self.raw), name);
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
    pub(crate) raw: <Backend as Types>::GraphicsPipeline,
}

/// A compiled compute shader, for dispatches.
pub struct ComputePipeline {
    pub(crate) raw: <Backend as Types>::ComputePipeline,
}

/// Compiled amplification, mesh and pixel shaders plus fixed-function state, for mesh dispatches.
pub struct MeshPipeline {
    pub(crate) raw: <Backend as Types>::MeshPipeline,
}

macro_rules! named {
    ($($ty:ident => $variant:ident),*) => {$(
        impl $ty {
            /// Names it for debuggers (PIX, RenderDoc) and crash reports.
            pub fn set_name(&self, name: &str) {
                Backend::set_name(Object::$variant(&self.raw), name);
            }
        }
    )*};
}

named!(
    GraphicsPipeline => GraphicsPipeline,
    ComputePipeline => ComputePipeline,
    MeshPipeline => MeshPipeline,
    QueryPool => QueryPool,
    Heap => Heap,
    AccelerationStructure => AccelerationStructure
);

/// Compiled pipelines kept between runs (D3D12 pipeline library, `VkPipelineCache`), so a
/// pipeline the driver compiled once loads instead of compiling again: the difference between a
/// smooth first frame and a stutter per new material.
///
/// Pass it in a pipeline description's `cache`; pipelines are looked up by their whole
/// description and stored when missing. Save [`serialize`](Self::serialize)'s bytes at exit (or
/// after loading screens) and feed them to [`Device::create_pipeline_cache`] next run. Safe to
/// use from several threads at once.
pub struct PipelineCache {
    pub(crate) raw: <Backend as Types>::PipelineCache,
}

impl PipelineCache {
    /// Everything stored so far, for [`Device::create_pipeline_cache`] on a later run.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        Backend::serialize_pipeline_cache(&self.raw)
    }
}

/// GPU queries of one kind.
pub struct QueryPool {
    pub(crate) raw: <Backend as Types>::QueryPool,
    pub(crate) desc: QueryPoolDesc,
}

impl QueryPool {
    #[inline]
    pub fn desc(&self) -> &QueryPoolDesc {
        &self.desc
    }
}

/// GPU memory that placed resources are created in.
pub struct Heap {
    pub(crate) raw: <Backend as Types>::Heap,
    pub(crate) desc: HeapDesc,
}

impl Heap {
    #[inline]
    pub fn desc(&self) -> &HeapDesc {
        &self.desc
    }
}

/// A bounding volume hierarchy ray queries traverse: bottom-level over triangles or boxes,
/// top-level over instances of bottom-level ones. Built on the GPU by
/// [`CommandList::build_acceleration_structure`]; synchronized through barriers on its
/// [`buffer`](Self::buffer) ([`BufferState::ACCELERATION_STRUCTURE_WRITE`] /
/// [`ACCELERATION_STRUCTURE_READ`](crate::BufferState::ACCELERATION_STRUCTURE_READ)).
///
/// [`BufferState::ACCELERATION_STRUCTURE_WRITE`]: crate::BufferState::ACCELERATION_STRUCTURE_WRITE
pub struct AccelerationStructure {
    pub(crate) raw: <Backend as Types>::AccelerationStructure,
    storage: Buffer,
    offset: u64,
    size: u64,
    kind: AccelerationStructureKind,
}

impl AccelerationStructure {
    #[inline]
    pub fn kind(&self) -> AccelerationStructureKind {
        self.kind
    }

    /// Bytes of storage.
    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The storage buffer holding it, shared with the other structures in it: for barriers and
    /// residency.
    #[inline]
    pub fn buffer(&self) -> &Buffer {
        &self.storage
    }

    /// Where in [`buffer`](Self::buffer) it starts.
    #[inline]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Its slot in the bindless resource table: `ResourceDescriptorHeap[index]` is a
    /// `RaytracingAccelerationStructure`.
    #[inline]
    pub fn index(&self) -> u32 {
        Backend::acceleration_structure_index(&self.raw)
    }

    /// What [`AccelerationStructureInstance`](crate::AccelerationStructureInstance) records
    /// name a bottom-level structure by (its GPU address).
    #[inline]
    pub fn instance_address(&self) -> u64 {
        Backend::acceleration_structure_address(&self.raw)
    }
}

/// An OS handle to a buffer, texture or fence another device, API or process can open
/// (Windows: an NT handle). Closed on drop.
pub struct SharedHandle {
    raw: <Backend as Types>::SharedHandle,
}

impl SharedHandle {
    /// The handle, still owned by `self`.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::shared_handle_raw(&self.raw)
    }

    /// Takes ownership of a handle another process duplicated into this one.
    ///
    /// # Safety
    /// `raw` is an owned handle of the kind the export functions return.
    pub unsafe fn from_raw(raw: *mut c_void) -> Self {
        Self {
            // SAFETY: forwarded from the caller.
            raw: unsafe { Backend::shared_handle_from_raw(raw) },
        }
    }
}

/// The images a window presents, rotated by the presentation engine.
///
/// Per frame: [`acquire`](Self::acquire) an image, record work that moves it from
/// [`TextureState::PRESENT`](crate::TextureState::PRESENT) to a render target and back, submit,
/// then [`present`](Self::present). Dropping the swapchain waits for its queue to go idle.
///
/// Flip-model presentation with a borderless window gets independent flip (exclusive
/// fullscreen's latency) without an exclusive mode, so there is none.
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

    /// The display can show `color_space` from this swapchain's format.
    pub fn supports_color_space(&self, color_space: ColorSpace) -> bool {
        validate_color_space(self.format(), color_space).is_ok()
            && Backend::supports_color_space(&self.raw, color_space)
    }

    /// Switches how the display reads the images (HDR on or off). `Unsupported` unless
    /// [`supports_color_space`](Self::supports_color_space).
    pub fn set_color_space(&mut self, color_space: ColorSpace) -> Result<()> {
        validate_color_space(self.format(), color_space)?;
        require(Backend::supports_color_space(&self.raw, color_space))?;
        Backend::set_color_space(&mut self.raw, color_space)
    }

    /// The HDR10 mastering metadata the display tone-maps with; `None` clears it.
    pub fn set_hdr_metadata(&mut self, metadata: Option<&HdrMetadata>) -> Result<()> {
        Backend::set_hdr_metadata(&mut self.raw, metadata)
    }

    #[inline]
    pub fn color_space(&self) -> ColorSpace {
        Backend::color_space(&self.raw)
    }

    /// The display the window is mostly on: whether it is in HDR, and its luminance range for
    /// tone mapping.
    pub fn output(&self) -> Result<OutputInfo> {
        Backend::containing_output(&self.raw)
    }

    /// When recent presents reached the display, for frame pacing. `None` before the first
    /// present reaches the display, or after a mode change resets the counters.
    pub fn frame_statistics(&self) -> Result<Option<FrameStatistics>> {
        Backend::frame_statistics(&self.raw)
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

    /// The native swapchain (D3D12: `IDXGISwapChain3*`), borrowed.
    pub fn as_raw(&self) -> *mut c_void {
        Backend::native(Object::Swapchain(&self.raw))
    }
}

/// Memory that command lists record into (D3D12 command allocator, `VkCommandPool`). One list
/// records from a pool at a time; reset it once the GPU is done with everything recorded.
pub struct CommandPool {
    pub(crate) raw: <Backend as Types>::CommandPool,
    /// The work its lists record.
    pub(crate) ty: QueueType,
    /// The queue they run on (graphics where there is no dedicated queue of `ty`).
    pub(crate) backend_ty: QueueType,
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
        unsafe { <Backend as crate::contract::Commands>::reset_pool(&mut self.raw) }
    }

    pub fn set_name(&self, name: &str) {
        Backend::set_name(Object::CommandPool(&self.raw), name);
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
    pub(crate) ty: QueueType,
    pub(crate) backend_ty: QueueType,
    pub(crate) features: Features,
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
    PipelineCache,
    SharedHandle,
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

impl core::fmt::Debug for AccelerationStructure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AccelerationStructure")
            .field("kind", &self.kind)
            .field("size", &self.size())
            .finish()
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
