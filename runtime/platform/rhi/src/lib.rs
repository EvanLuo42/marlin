//! Rendering hardware interface over D3D12, Vulkan, Metal and console GPU APIs.
//!
//! A thin, explicit layer in the style of D3D12 and Vulkan: the caller records barriers, owns
//! synchronization through fences, and keeps resources alive while the GPU uses them (those
//! points are `unsafe fn`s). State tracking, barrier derivation and deferred destruction belong
//! to the render graph above. [`FramesInFlight`] is the safe helper for the common frame loop.
//!
//! # Abstraction choices
//!
//! - **One backend per build**, chosen by `cfg(marlin_platform)` like marlin-pal: D3D12 on
//!   Windows and Xbox, Vulkan on Linux, Metal on macOS, the console APIs on consoles. No dynamic
//!   dispatch; the public types wrap the backend's associated types (`contract::Types`).
//! - **Timeline fences** (`u64` values) are the only CPU/GPU and queue/queue synchronization:
//!   D3D12 fences, Vulkan timeline semaphores and Metal shared events all have this shape.
//! - **Barriers are sync/access/layout triples** ([`TextureState`]): D3D12 enhanced barriers and
//!   Vulkan synchronization2 take them directly; D3D12 without enhanced barriers derives legacy
//!   resource states from the layout.
//! - **Render passes with load/store ops** bind targets and clear them: Vulkan dynamic rendering,
//!   Metal render passes and tile-based GPUs need them; D3D12 emulates them with
//!   `OMSetRenderTargets` + `ClearRenderTargetView`.
//! - **Swapchains pace the CPU** with a frame-latency wait in [`Swapchain::acquire`] (DXGI's
//!   waitable object, Vulkan's acquire). A submission's swapchain dependency will become explicit
//!   when the Vulkan backend needs its semaphores.
//! - **Bindless only**: buffer and texture views ([`ShaderView`]) and samplers occupy slots in
//!   device-wide tables, and shaders index those tables with values passed in push constants or
//!   buffers. There are no descriptor sets, root tables or vertex input layouts; every pipeline
//!   shares one layout. Baseline: D3D12 feature level 12_0, shader model 6.6, resource binding
//!   tier 3.
//! - **Shaders are bytecode** ([`ShaderModule`]): DXIL, SPIR-V or a console format, produced
//!   offline. The RHI never compiles source. `docs/design/0002-shader-interface.md` is the
//!   contract shaders follow.
//!
//! Validation messages (D3D12 debug layer) go to the `marlin_diag` log, and
//! [`Device::validation_errors`] counts errors.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod api;
mod backend;
mod contract;
mod error;
mod frames;
mod types;

pub use api::{
    Buffer, CommandList, CommandPool, ComputePipeline, DepthStencilView, Device, Fence,
    GraphicsPipeline, Instance, Queue, RenderTargetView, Sampler, ShaderView, Swapchain, Texture,
};
pub use error::{Error, ErrorKind, Result};
pub use frames::FramesInFlight;
pub use types::{
    Access, AdapterInfo, AdapterKind, AdapterPreference, AddressMode, BlendComponent, BlendFactor,
    BlendOp, BlendState, BorderColor, BufferBarrier, BufferDesc, BufferState, BufferTextureCopy,
    BufferUsage, BufferViewDesc, BufferViewKind, ClearValue, ColorAttachment, ColorTargetState,
    ColorWrites, CompareOp, ComputePipelineDesc, CullMode, DepthAttachment, DepthState,
    DepthStencilViewDesc, DeviceDesc, Features, Filter, Format, FrontFace, GraphicsPipelineDesc,
    IndexFormat, InstanceDesc, Layout, LoadOp, MAX_BINDLESS_RESOURCES, MAX_BINDLESS_SAMPLERS,
    MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MAX_UNIFORM_SIZE, MemoryLocation, PolygonMode,
    PresentMode, PresentStatus, PrimitiveTopology, QueueType, RasterState, Rect, RenderPassDesc,
    RenderTargetViewDesc, SamplerDesc, ShaderFormat, ShaderModule, Stage, StoreOp,
    SubresourceRange, SurfaceHandle, SwapchainDesc, TEXTURE_OFFSET_ALIGNMENT,
    TEXTURE_ROW_PITCH_ALIGNMENT, TextureBarrier, TextureDesc, TextureState, TextureUsage,
    TextureViewDesc, TextureViewDimension, UNIFORM_ALIGNMENT, Viewport, aligned_row_pitch,
};

/// The shader bytecode this build's backend consumes. Engine code picks shader blobs with it
/// rather than by platform.
pub const SHADER_FORMAT: ShaderFormat = <backend::imp::Backend as contract::Types>::SHADER_FORMAT;

// The selected backend must implement the whole contract.
const _: () = {
    const fn assert_rhi<R: contract::Rhi>() {}
    assert_rhi::<backend::imp::Backend>();
};
