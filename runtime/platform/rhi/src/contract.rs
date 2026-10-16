//! The backend contract.
//!
//! A backend is a unit type `Backend` implementing [`Rhi`]: associated types for its objects and
//! static functions on them, the same shape as marlin-pal's contract. The public types in
//! `api.rs` wrap the associated types and forward here, so a backend missing an item or getting a
//! signature wrong fails to compile on its own platform.
//!
//! Backends may name the public wrapper types (`crate::Texture`, ...) where the contract hands
//! them out by reference: a swapchain owns its `crate::Texture`s so it can lend `&Texture`.

use core::time::Duration;

use crate::{
    AdapterInfo, CommandList, DeviceDesc, Features, Fence, Format, InstanceDesc, PresentMode,
    PresentStatus, RenderPassDesc, RenderTargetView, Result, SurfaceHandle, SwapchainDesc, Texture,
    TextureBarrier,
};

pub(crate) trait Types {
    type Instance: Send + Sync;
    type Device: Send + Sync;
    type Queue: Send + Sync;
    type Fence: Send + Sync;
    type CommandPool: Send;
    type CommandList: Send;
    type Swapchain: Send;
    type Texture: Send + Sync;
    type RenderTargetView: Send + Sync;
}

pub(crate) trait Instance: Types {
    fn create_instance(desc: &InstanceDesc) -> Result<Self::Instance>;
    /// The adapter at `index` in enumeration order (highest performance first), or `None` past
    /// the end.
    fn adapter(instance: &Self::Instance, index: u32) -> Option<AdapterInfo>;
    /// Creates the device and its graphics queue.
    fn create_device(
        instance: &Self::Instance,
        desc: &DeviceDesc,
    ) -> Result<(Self::Device, Self::Queue)>;
}

pub(crate) trait Device: Types {
    fn adapter_info(device: &Self::Device) -> &AdapterInfo;
    fn features(device: &Self::Device) -> Features;
    fn create_fence(device: &Self::Device, initial: u64) -> Result<Self::Fence>;
    fn create_command_pool(device: &Self::Device) -> Result<Self::CommandPool>;
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
}

pub(crate) trait Queue: Types {
    /// # Safety
    /// Every resource the lists use stays alive, and every list and its pool stay unreset, until
    /// the GPU finishes them.
    unsafe fn submit(
        queue: &Self::Queue,
        lists: &[&CommandList],
        signal: Option<(&Fence, u64)>,
    ) -> Result<()>;
    /// Blocks until all submitted work has finished.
    fn wait_idle(queue: &Self::Queue) -> Result<()>;
}

pub(crate) trait Fences: Types {
    fn completed_value(fence: &Self::Fence) -> u64;
    /// `Ok(false)` on timeout.
    fn wait(fence: &Self::Fence, value: u64, timeout: Option<Duration>) -> Result<bool>;
    fn signal(fence: &Self::Fence, value: u64) -> Result<()>;
}

pub(crate) trait Commands: Types {
    /// # Safety
    /// The GPU has finished every list recorded from the pool.
    unsafe fn reset_pool(pool: &mut Self::CommandPool) -> Result<()>;
    fn begin(list: &mut Self::CommandList, pool: &mut Self::CommandPool) -> Result<()>;
    fn end(list: &mut Self::CommandList) -> Result<()>;
    fn texture_barriers(list: &mut Self::CommandList, barriers: &[TextureBarrier<'_>]);
    fn begin_render_pass(list: &mut Self::CommandList, desc: &RenderPassDesc<'_>);
    fn end_render_pass(list: &mut Self::CommandList);
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
pub(crate) trait Rhi: Instance + Device + Queue + Fences + Commands + Swapchain {}
