//! Rendering hardware interface over Vulkan, D3D12, Metal and console GPU APIs.
//!
//! A thin, explicit layer in the style of Vulkan and D3D12: the caller records barriers, owns
//! synchronization through fences, and keeps resources alive while the GPU uses them (those
//! points are `unsafe fn`s). State tracking, barrier derivation and deferred destruction belong
//! to the render graph above. [`FramesInFlight`] keeps the common frame loop's
//! command pools, lists and fence safe; what the lists use is still the caller's.
//!
//! # Abstraction choices
//!
//! - **One backend per build**, chosen by `cfg(marlin_platform)` like marlin-pal: Vulkan on
//!   Windows and Linux, D3D12 on Xbox, Metal on macOS, the console APIs on consoles. No dynamic
//!   dispatch; the public types wrap the backend's associated types (`contract::Types`). Code
//!   that depends on the backend asks [`GRAPHICS_API`] and [`SHADER_FORMAT`], never the platform.
//! - **Timeline fences** (`u64` values) are the only CPU/GPU and queue/queue synchronization:
//!   Vulkan timeline semaphores, D3D12 fences and Metal shared events all have this shape.
//! - **Barriers are sync/access/layout triples** ([`TextureState`]): Vulkan synchronization2 and
//!   D3D12 enhanced barriers take them directly; D3D12 without enhanced barriers derives legacy
//!   resource states from the layout, and the caller names the real layout behind
//!   [`Layout::Undefined`] ([`TextureBarrier::undefined_from`]).
//! - **Render passes with load/store ops** bind targets and clear them: Vulkan dynamic rendering,
//!   Metal render passes and tile-based GPUs need them; D3D12 emulates them with
//!   `OMSetRenderTargets` + `ClearRenderTargetView`. Multisample resolves are explicit commands.
//! - **Swapchains pace the CPU** with a frame-latency wait in [`Swapchain::acquire`] (Vulkan's
//!   present wait, DXGI's waitable object). The dependency between acquiring an image and the
//!   work that renders it stays implicit: the next submission on the swapchain's queue waits for
//!   it (Vulkan's acquire semaphore).
//! - **Bindless only**: buffer and texture views ([`ShaderView`]), acceleration structures and
//!   samplers occupy slots in device-wide tables, and shaders index those tables with values
//!   passed in push constants or buffers. There are no descriptor sets, root tables or vertex
//!   input layouts; every pipeline shares one layout. Baseline: mesh shaders and bindless tables
//!   on every backend (Vulkan: 1.4 with descriptor heaps, mesh shaders and device-generated
//!   commands; D3D12: feature level 12_0, shader model 6.6, resource binding tier 3).
//! - **Typed queues**: graphics always, plus dedicated compute and copy queues where the device
//!   has them. Command pools and lists carry their queue type; submissions wait on fences on the
//!   GPU.
//! - **Indirect commands set one push constant** ([`INDIRECT_CONSTANT_WORD`]) per record, so one
//!   multi-draw runs many differently parameterized draws (Vulkan device-generated commands,
//!   D3D12 command signatures).
//! - **Memory** is committed per resource, placed in [`Heap`]s where resources may alias, or
//!   bound tile by tile to sparse resources. Sub-allocation policy lives above the RHI; the RHI
//!   reports the OS memory budget and pages heaps in and out on request.
//! - **Ray tracing is inline ray queries** over [`AccelerationStructure`]s, from any shader. There
//!   are no ray tracing pipelines or shader tables.
//! - **Shaders are bytecode** ([`ShaderModule`]): SPIR-V, DXIL or a console format
//!   ([`SHADER_FORMAT`]), produced offline. The RHI never compiles source.
//!   `docs/design/0002-shader-interface.md` is the contract shaders follow. [`PipelineCache`]
//!   keeps the driver's compiled pipelines between runs.
//! - **Native escape hatches**: every object hands out its API object (`as_raw`) and textures and
//!   buffers can wrap foreign ones, for libraries that speak the native API (upscalers, XR
//!   runtimes, vendor SDKs). Resources, fences and buffers can also be shared across devices and
//!   processes through OS handles ([`SharedHandle`]).
//!
//! # Diagnostics
//!
//! Validation messages (the Vulkan validation layer, the D3D12 debug layer; optionally their
//! GPU-based validation) go to the `marlin_diag` log, and [`Device::validation_errors`] counts
//! errors. Objects carry debug names (`set_name`) for GPU debuggers (RenderDoc, Nsight, PIX) and
//! crash reports. A lost device is reported as [`ErrorKind::DeviceLost`] by every call that
//! touches the GPU; with [`InstanceDesc::crash_diagnostics`] the first report logs the command
//! lists in flight, the last operation each reached, and the faulting address with the resources
//! around it (Vulkan device fault and checkpoints, D3D12 DRED).
//! [`CommandList::write_marker`] adds the engine's own breadcrumbs.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod api;
mod backend;
mod commands;
mod contract;
mod error;
mod frames;
mod types;

pub use api::{
    AccelerationStructure, Buffer, CommandList, CommandPool, ComputePipeline, DepthStencilView,
    Device, Fence, GraphicsPipeline, Heap, Instance, MeshPipeline, PipelineCache, QueryPool, Queue,
    RenderTargetView, Sampler, ShaderView, SharedHandle, Swapchain, Texture,
};
pub use commands::{MAX_VIEWPORTS, MEMORY_BARRIER_ACCESS};
pub use error::{Error, ErrorKind, Result};
pub use frames::FramesInFlight;
pub use types::{
    ACCELERATION_STRUCTURE_ALIGNMENT, AccelerationStructureAabbs, AccelerationStructureCopy,
    AccelerationStructureGeometry, AccelerationStructureInputs, AccelerationStructureInstance,
    AccelerationStructureKind, AccelerationStructureSizes, AccelerationStructureTriangles, Access,
    AdapterInfo, AdapterKind, AdapterPreference, AddressMode, BlendComponent, BlendFactor, BlendOp,
    BlendState, BorderColor, BufferBarrier, BufferDesc, BufferState, BufferTextureCopy,
    BufferUsage, BufferViewDesc, BufferViewKind, BuildAccelerationStructure, BuildFlags,
    ClearColor, ClearValue, ClockCalibration, ColorAttachment, ColorSpace, ColorTargetState,
    ColorWrites, CompareOp, ComputePipelineDesc, CullMode, DepthAttachment, DepthState,
    DepthStencilViewDesc, DeviceDesc, DispatchIndirectArgs, DrawIndexedIndirectArgs,
    DrawIndirectArgs, Features, FenceWait, Filter, Format, FormatFeatures, FormatSupport,
    FrameStatistics, FrontFace, GeometryFlags, GraphicsApi, GraphicsPipelineDesc, HdrMetadata,
    HeapDesc, HeapKind, INDIRECT_CONSTANT_WORD, INSTANCE_ALIGNMENT, IndexFormat, IndirectArgs,
    InstanceDesc, InstanceFlags, Layout, LoadOp, MAX_BINDLESS_RESOURCES, MAX_BINDLESS_SAMPLERS,
    MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MAX_UNIFORM_SIZE, MSAA_PLACEMENT_ALIGNMENT,
    MarkerPoint, MemoryBudget, MemoryLocation, MemoryRequirements, MemoryUsage, MeshPipelineDesc,
    MultisampleState, OutputInfo, PLACEMENT_ALIGNMENT, Pageable, PipelineStatistics, PolygonMode,
    PredicateSkip, PresentMode, PresentStatus, PrimitiveTopology, QueryKind, QueryPoolDesc,
    QueueType, RasterState, Rect, RenderPassDesc, RenderTargetViewDesc, ResidencyPriority,
    SPARSE_TILE_SIZE, SamplerDesc, SamplerReduction, ShaderFormat, ShaderModule, ShadingRate,
    ShadingRateCombiner, SparseBind, SparseTextureLayout, SparseTextureRegion, Stage, StencilFace,
    StencilOp, StencilState, StoreOp, SubmitDesc, SubresourceRange, SurfaceHandle, SwapchainDesc,
    TEXTURE_OFFSET_ALIGNMENT, TEXTURE_ROW_PITCH_ALIGNMENT, TextureBarrier, TextureCopy,
    TextureDesc, TextureDimension, TextureResolve, TextureState, TextureUsage, TextureViewDesc,
    TextureViewDimension, UNIFORM_ALIGNMENT, VariableRateShading, VertexFormat, Viewport,
    aligned_row_pitch,
};

/// The shader bytecode this build's backend consumes. Engine code picks shader blobs with it
/// rather than by platform.
pub const SHADER_FORMAT: ShaderFormat = <backend::imp::Backend as contract::Types>::SHADER_FORMAT;

/// The GPU API this build's backend drives.
pub const GRAPHICS_API: GraphicsApi = <backend::imp::Backend as contract::Types>::GRAPHICS_API;

// The selected backend must implement the whole contract.
const _: () = {
    const fn assert_rhi<R: contract::Rhi>() {}
    assert_rhi::<backend::imp::Backend>();
};
