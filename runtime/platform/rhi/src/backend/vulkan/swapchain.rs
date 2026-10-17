//! Surfaces, swapchains and presentation (0016 §7.1), and display outputs.

use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::device::{Device, Shared};
use super::ffi::*;
use super::instance::{Instance, InstanceShared};
use super::queue::Queue;
use crate::{
    ColorSpace, Format, FrameStatistics, HdrMetadata, OutputInfo, PresentMode, PresentStatus,
    Result, SurfaceHandle, SwapchainDesc,
};

/// Whether the device can present without vertical sync on this system (`Features::tearing`).
pub(crate) fn tearing(instance: &InstanceShared, physical: VkPhysicalDevice) -> bool {
    let _ = (instance, physical);
    false
}

/// Display `output` of adapter `adapter`, or `None` past the end.
pub(crate) fn output(instance: &Instance, adapter: u32, output: u32) -> Option<OutputInfo> {
    let _ = (instance, adapter, output);
    None
}

pub(crate) struct Swapchain {
    shared: Arc<Shared>,
    raw: VkSwapchainKHR,
    textures: Vec<crate::Texture>,
    views: Vec<crate::RenderTargetView>,
}

impl Swapchain {
    /// # Safety
    /// `surface` is a live window that outlives the swapchain.
    pub(crate) unsafe fn new(
        device: &Device,
        queue: &Queue,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<Self> {
        let _ = (device, queue, surface, desc);
        todo!("vulkan: create_swapchain")
    }

    pub(crate) fn raw(&self) -> VkSwapchainKHR {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    pub(crate) fn acquire(&mut self, timeout: Option<Duration>) -> Result<u32> {
        let _ = timeout;
        todo!("vulkan: acquire")
    }

    pub(crate) fn present(&mut self) -> Result<PresentStatus> {
        todo!("vulkan: present")
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let _ = (width, height);
        todo!("vulkan: resize")
    }

    pub(crate) fn set_present_mode(&mut self, mode: PresentMode) {
        let _ = mode;
        todo!("vulkan: set_present_mode")
    }

    pub(crate) fn supports_color_space(&self, color_space: ColorSpace) -> bool {
        let _ = color_space;
        todo!("vulkan: supports_color_space")
    }

    pub(crate) fn set_color_space(&mut self, color_space: ColorSpace) -> Result<()> {
        let _ = color_space;
        todo!("vulkan: set_color_space")
    }

    pub(crate) fn set_hdr_metadata(&mut self, metadata: Option<&HdrMetadata>) -> Result<()> {
        let _ = metadata;
        todo!("vulkan: set_hdr_metadata")
    }

    pub(crate) fn containing_output(&self) -> Result<OutputInfo> {
        todo!("vulkan: containing_output")
    }

    pub(crate) fn frame_statistics(&self) -> Result<Option<FrameStatistics>> {
        todo!("vulkan: frame_statistics")
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        todo!("vulkan: size")
    }

    pub(crate) fn format(&self) -> Format {
        todo!("vulkan: format")
    }

    pub(crate) fn color_space(&self) -> ColorSpace {
        todo!("vulkan: color_space")
    }

    pub(crate) fn textures(&self) -> &[crate::Texture] {
        &self.textures
    }

    pub(crate) fn views(&self) -> &[crate::RenderTargetView] {
        &self.views
    }
}
