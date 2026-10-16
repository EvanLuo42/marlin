//! Placeholder for platforms whose backend is not written yet: [`Instance`] creation fails with
//! `Unsupported`, so no other object can exist.

use core::time::Duration;

use crate::contract::{Commands, Device, Fences, Instance, Queue, Rhi, Swapchain, Types};
use crate::{
    AdapterInfo, CommandList, DeviceDesc, Error, ErrorKind, Features, Fence, Format, InstanceDesc,
    PresentMode, PresentStatus, RenderPassDesc, RenderTargetView, Result, SurfaceHandle,
    SwapchainDesc, Texture, TextureBarrier,
};

pub(crate) struct Backend;

pub(crate) enum Never {}

impl Types for Backend {
    type Instance = Never;
    type Device = Never;
    type Queue = Never;
    type Fence = Never;
    type CommandPool = Never;
    type CommandList = Never;
    type Swapchain = Never;
    type Texture = Never;
    type RenderTargetView = Never;
}

impl Rhi for Backend {}

impl Instance for Backend {
    fn create_instance(_: &InstanceDesc) -> Result<Never> {
        Err(Error::from_kind(ErrorKind::Unsupported))
    }

    fn adapter(instance: &Never, _: u32) -> Option<AdapterInfo> {
        match *instance {}
    }

    fn create_device(instance: &Never, _: &DeviceDesc) -> Result<(Never, Never)> {
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

    fn create_command_pool(device: &Never) -> Result<Never> {
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
}

impl Queue for Backend {
    unsafe fn submit(queue: &Never, _: &[&CommandList], _: Option<(&Fence, u64)>) -> Result<()> {
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

    fn begin_render_pass(list: &mut Never, _: &RenderPassDesc<'_>) {
        match *list {}
    }

    fn end_render_pass(list: &mut Never) {
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
