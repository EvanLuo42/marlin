//! Surfaces, swapchains and presentation (0016 §7.1), and display outputs.
//!
//! Synchronization: `acquire` signals a binary semaphore that the swapchain queue's next submit
//! waits on (the queue's pending waits). `present` submits an empty batch on that queue that
//! waits on it if no submit took it, and signals the image's present semaphore (its first
//! synchronization scope covers every earlier submission) and the swapchain's frame timeline;
//! then it presents, waiting on the present semaphore. An acquire semaphore is reused once the
//! timeline passed the present that followed it. Present semaphores are per image: re-acquiring
//! an image means the presentation engine is done with its previous present.
//!
//! The app sees each image in `PRESENT_SRC_KHR` (`Layout::Present`), as on D3D12: the first
//! acquire of an image transitions it from `UNDEFINED` in a submission of its own.
//!
//! Frame pacing: `acquire` blocks until at most `max_frame_latency` frames are queued: with
//! `VK_KHR_present_wait2`, until frame `n - latency` reached the display; without, until the GPU
//! finished it.
//!
//! Sizes: Win32 surfaces only take swapchains of the window's size, unless the presentation
//! engine scales (`VK_KHR_surface_maintenance1` + `VK_KHR_swapchain_maintenance1`). Then any size
//! is stretched to the window as DXGI does; without, the swapchain takes the window's size.
//! Present modes the chosen one is compatible with are switched per present (maintenance1);
//! others, color spaces and lost (out-of-date) swapchains recreate the `VkSwapchainKHR`.

use core::ops::Deref;
use core::ptr;
use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::device::{Device, Shared};
use super::ffi::*;
use super::instance::{Instance, InstanceShared, enumerate};
use super::queue::{Queue, QueueHandle};
use super::resource::{self, Texture as RawTexture};
use super::{check, error, unsupported};
use crate::{
    ColorSpace, Error, ErrorKind, Format, FrameStatistics, HdrMetadata, OutputInfo, PresentMode,
    PresentStatus, RenderTargetViewDesc, Result, SurfaceHandle, SwapchainDesc, TextureDesc,
    TextureUsage,
};

/// Whether the device can present without vertical sync on this system (`Features::tearing`):
/// a surface on a hidden probe window offers `IMMEDIATE`.
pub(crate) fn tearing(instance: &InstanceShared, physical: VkPhysicalDevice) -> bool {
    #[cfg(marlin_platform = "windows")]
    {
        let Some(window) = win32::ProbeWindow::new() else {
            return false;
        };
        // SAFETY: the window lives until the end of this block, past the surface.
        let Ok(surface) = (unsafe { Surface::new(instance, window.handle()) }) else {
            return false;
        };
        surface.supported_by(physical)
            && surface
                .present_modes(physical)
                .contains(&VK_PRESENT_MODE_IMMEDIATE_KHR)
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        let _ = (instance, physical);
        false
    }
}

/// Display `output` of adapter `adapter`, or `None` past the end. Vulkan enumerates no
/// displays; the OS's are matched to the adapter by LUID.
pub(crate) fn output(instance: &Instance, adapter: u32, output: u32) -> Option<OutputInfo> {
    let info = instance.adapter(adapter)?;
    #[cfg(marlin_platform = "windows")]
    {
        win32::output(info.luid, output)
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        let _ = (info, output);
        None
    }
}

fn timeout_ns(timeout: Option<Duration>) -> u64 {
    timeout.map_or(u64::MAX, |t| t.as_nanos().min(u64::MAX as u128 - 1) as u64)
}

fn timed_out() -> Error {
    Error::from_kind(ErrorKind::TimedOut)
}

/// The image format of a presentable `Format`. Vulkan surfaces take sRGB formats directly.
fn surface_format(format: Format) -> Option<VkFormat> {
    Some(match format {
        Format::Rgba8Unorm => VK_FORMAT_R8G8B8A8_UNORM,
        Format::Rgba8UnormSrgb => VK_FORMAT_R8G8B8A8_SRGB,
        Format::Bgra8Unorm => VK_FORMAT_B8G8R8A8_UNORM,
        Format::Bgra8UnormSrgb => VK_FORMAT_B8G8R8A8_SRGB,
        Format::Rgb10a2Unorm => VK_FORMAT_A2B10G10R10_UNORM_PACK32,
        Format::Rgba16Float => VK_FORMAT_R16G16B16A16_SFLOAT,
        _ => return None,
    })
}

fn vk_color_space(color_space: ColorSpace) -> VkColorSpaceKHR {
    match color_space {
        ColorSpace::Srgb => VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        ColorSpace::Hdr10 => VK_COLOR_SPACE_HDR10_ST2084_EXT,
        ColorSpace::ScRgb => VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT,
    }
}

/// The modes `mode` may use, best first: `Immediate` falls back to `Mailbox` (no tearing), both
/// to `Fifo` (always there).
fn candidates(mode: PresentMode) -> &'static [VkPresentModeKHR] {
    match mode {
        PresentMode::Fifo => &[VK_PRESENT_MODE_FIFO_KHR],
        PresentMode::Mailbox => &[VK_PRESENT_MODE_MAILBOX_KHR, VK_PRESENT_MODE_FIFO_KHR],
        PresentMode::Immediate => &[
            VK_PRESENT_MODE_IMMEDIATE_KHR,
            VK_PRESENT_MODE_MAILBOX_KHR,
            VK_PRESENT_MODE_FIFO_KHR,
        ],
    }
}

fn resolve(mode: PresentMode, supported: &[VkPresentModeKHR]) -> VkPresentModeKHR {
    candidates(mode)
        .iter()
        .copied()
        .find(|m| supported.contains(m))
        .unwrap_or(VK_PRESENT_MODE_FIFO_KHR)
}

// --- Surfaces ---

/// A `VkSurfaceKHR`, destroyed on drop.
struct Surface<I: Deref<Target = InstanceShared> = Arc<InstanceShared>> {
    instance: I,
    raw: VkSurfaceKHR,
}

/// What a surface allows for one present mode.
struct SurfaceCaps {
    caps: VkSurfaceCapabilitiesKHR,
    /// The extent range the presentation engine stretches to the window, if it does.
    scaled: Option<(VkExtent2D, VkExtent2D)>,
    /// Scaling may anchor at the top left (else no gravity is given).
    gravity: bool,
    /// Modes a swapchain created with this one may switch to per present.
    compatible: Vec<VkPresentModeKHR>,
    present_wait: bool,
}

impl<I: Deref<Target = InstanceShared>> Surface<I> {
    /// # Safety
    /// `surface` is a live window that outlives the surface.
    unsafe fn new(instance: I, surface: SurfaceHandle) -> Result<Self> {
        let SurfaceHandle::Win32 { hwnd, hinstance } = surface;
        let create = instance
            .fns
            .vkCreateWin32SurfaceKHR
            .filter(|_| instance.ext.surface && instance.ext.win32_surface)
            .ok_or_else(unsupported)?;
        let info = VkWin32SurfaceCreateInfoKHR {
            hinstance: hinstance.as_ptr(),
            hwnd: hwnd.as_ptr(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: the caller guarantees the window; valid create info and out pointer.
        check(unsafe { create(instance.raw, &info, ptr::null(), &mut raw) })?;
        Ok(Self { instance, raw })
    }

    /// Some queue family of `physical` presents to the surface.
    fn supported_by(&self, physical: VkPhysicalDevice) -> bool {
        let f = &self.instance.fns;
        let Some(support) = f.vkGetPhysicalDeviceSurfaceSupportKHR else {
            return false;
        };
        let mut count = 0;
        // SAFETY: the two-call idiom's first call.
        unsafe {
            (f.vkGetPhysicalDeviceQueueFamilyProperties)(physical, &mut count, ptr::null_mut())
        };
        (0..count).any(|family| {
            let mut on = VK_FALSE;
            // SAFETY: a live surface, a family index in range, a valid out pointer.
            (unsafe { support(physical, family, self.raw, &mut on) }) == VK_SUCCESS
                && on != VK_FALSE
        })
    }

    fn supports_family(&self, physical: VkPhysicalDevice, family: u32) -> bool {
        let Some(support) = self.instance.fns.vkGetPhysicalDeviceSurfaceSupportKHR else {
            return false;
        };
        let mut on = VK_FALSE;
        // SAFETY: a live surface, a family of the device, a valid out pointer.
        (unsafe { support(physical, family, self.raw, &mut on) }) == VK_SUCCESS && on != VK_FALSE
    }

    fn present_modes(&self, physical: VkPhysicalDevice) -> Vec<VkPresentModeKHR> {
        let Some(get) = self.instance.fns.vkGetPhysicalDeviceSurfacePresentModesKHR else {
            return Vec::new();
        };
        // SAFETY: the two-call idiom on a live surface supported by the device.
        unsafe { enumerate(|n, p| get(physical, self.raw, n, p)) }
    }

    fn formats(&self, physical: VkPhysicalDevice) -> Vec<VkSurfaceFormatKHR> {
        let Some(get) = self.instance.fns.vkGetPhysicalDeviceSurfaceFormatsKHR else {
            return Vec::new();
        };
        // SAFETY: as above.
        unsafe { enumerate(|n, p| get(physical, self.raw, n, p)) }
    }

    /// The surface's capabilities with `mode` (`features`: what the device can use of them).
    fn caps(
        &self,
        physical: VkPhysicalDevice,
        mode: VkPresentModeKHR,
        features: &SwapFeatures,
    ) -> Result<SurfaceCaps> {
        let f = &self.instance.fns;
        let Some(get2) = f
            .vkGetPhysicalDeviceSurfaceCapabilities2KHR
            .filter(|_| features.caps2)
        else {
            let get = f
                .vkGetPhysicalDeviceSurfaceCapabilitiesKHR
                .ok_or_else(unsupported)?;
            let mut caps = VkSurfaceCapabilitiesKHR::default();
            // SAFETY: a live surface supported by the device and a valid out pointer.
            check(unsafe { get(physical, self.raw, &mut caps) })?;
            return Ok(SurfaceCaps {
                caps,
                scaled: None,
                gravity: false,
                compatible: Vec::new(),
                present_wait: false,
            });
        };
        let mut wait2 = VkSurfaceCapabilitiesPresentWait2KHR::default();
        let mut id2 = VkSurfaceCapabilitiesPresentId2KHR::default();
        let mut modes = [0; 8];
        let mut compatible = VkSurfacePresentModeCompatibilityKHR {
            presentModeCount: modes.len() as u32,
            pPresentModes: modes.as_mut_ptr(),
            ..Default::default()
        };
        let mut scaling = VkSurfacePresentScalingCapabilitiesKHR::default();
        let mut next: *mut core::ffi::c_void = ptr::null_mut();
        if features.present_wait {
            wait2.pNext = next;
            id2.pNext = (&raw mut wait2).cast();
            next = (&raw mut id2).cast();
        }
        let mut mode_info = VkSurfacePresentModeKHR {
            presentMode: mode,
            ..Default::default()
        };
        if features.maintenance1 {
            compatible.pNext = next;
            scaling.pNext = (&raw mut compatible).cast();
            next = (&raw mut scaling).cast();
        }
        let info = VkPhysicalDeviceSurfaceInfo2KHR {
            pNext: match features.maintenance1 {
                true => (&raw mut mode_info).cast_const().cast(),
                false => ptr::null(),
            },
            surface: self.raw,
            ..Default::default()
        };
        let mut caps2 = VkSurfaceCapabilities2KHR {
            pNext: next,
            ..Default::default()
        };
        // SAFETY: a live surface supported by the device and valid chains.
        check(unsafe { get2(physical, &info, &mut caps2) })?;
        let stretch = scaling.supportedPresentScaling & VK_PRESENT_SCALING_STRETCH_BIT_KHR != 0;
        let count = (compatible.presentModeCount as usize).min(modes.len());
        Ok(SurfaceCaps {
            caps: caps2.surfaceCapabilities,
            scaled: (features.maintenance1 && stretch)
                .then_some((scaling.minScaledImageExtent, scaling.maxScaledImageExtent)),
            gravity: scaling.supportedPresentGravityX & VK_PRESENT_GRAVITY_MIN_BIT_KHR != 0
                && scaling.supportedPresentGravityY & VK_PRESENT_GRAVITY_MIN_BIT_KHR != 0,
            compatible: modes[..count].to_vec(),
            present_wait: features.present_wait
                && id2.presentId2Supported != VK_FALSE
                && wait2.presentWait2Supported != VK_FALSE,
        })
    }
}

impl<I: Deref<Target = InstanceShared>> Drop for Surface<I> {
    fn drop(&mut self) {
        if let Some(destroy) = self.instance.fns.vkDestroySurfaceKHR {
            // SAFETY: no swapchain uses the surface any more.
            unsafe { destroy(self.instance.raw, self.raw, ptr::null()) };
        }
    }
}

/// The swapchain features the device has enabled (device.rs enables what it supports).
#[derive(Clone, Copy)]
struct SwapFeatures {
    /// `vkGetPhysicalDeviceSurfaceCapabilities2KHR`.
    caps2: bool,
    /// Present scaling and per-present mode switches.
    maintenance1: bool,
    /// Present ids and waits.
    present_wait: bool,
}

impl SwapFeatures {
    fn query(shared: &Shared) -> Self {
        let instance = &shared.instance;
        let ext = &shared.ext;
        let mut maintenance1 = VkPhysicalDeviceSwapchainMaintenance1FeaturesKHR::default();
        let mut id2 = VkPhysicalDevicePresentId2FeaturesKHR::default();
        let mut wait2 = VkPhysicalDevicePresentWait2FeaturesKHR::default();
        let mut next: *mut core::ffi::c_void = ptr::null_mut();
        if ext.swapchain_maintenance1 {
            maintenance1.pNext = next;
            next = (&raw mut maintenance1).cast();
        }
        if ext.present_wait2 {
            id2.pNext = next;
            wait2.pNext = (&raw mut id2).cast();
            next = (&raw mut wait2).cast();
        }
        let mut features = VkPhysicalDeviceFeatures2 {
            pNext: next,
            ..Default::default()
        };
        // SAFETY: a valid output chain of extensions the device has.
        unsafe { (instance.fns.vkGetPhysicalDeviceFeatures2)(shared.physical, &mut features) };
        let caps2 = instance.ext.get_surface_capabilities2
            && instance
                .fns
                .vkGetPhysicalDeviceSurfaceCapabilities2KHR
                .is_some();
        Self {
            caps2,
            maintenance1: caps2
                && instance.ext.surface_maintenance1
                && ext.swapchain_maintenance1
                && maintenance1.swapchainMaintenance1 != VK_FALSE,
            present_wait: caps2
                && ext.present_wait2
                && id2.presentId2 != VK_FALSE
                && wait2.presentWait2 != VK_FALSE
                && shared.fns.vkWaitForPresent2KHR.is_some(),
        }
    }
}

// --- Swapchains ---

/// The image acquired and not yet presented.
struct Acquired {
    index: u32,
    /// The semaphore `vkAcquireNextImageKHR` signalled.
    semaphore: VkSemaphore,
    /// The one on the queue's pending waits: `semaphore`, or the first-use transition's.
    wait: VkSemaphore,
}

/// What `frame_statistics` reads: the newest present known to have reached the display.
#[derive(Default)]
struct Displayed {
    /// Its present id (this `VkSwapchainKHR`'s presents, from 1).
    id: u64,
    /// The vertical blank count when it was seen done.
    refresh: u64,
}

pub(crate) struct Swapchain {
    shared: Arc<Shared>,
    queue: QueueHandle,
    /// The window, for its monitor (`containing_output`).
    hwnd: usize,
    surface: Surface,
    raw: VkSwapchainKHR,
    features: SwapFeatures,

    // What the app asked for.
    width: u32,
    height: u32,
    format: Format,
    vk_format: VkFormat,
    color_space: ColorSpace,
    image_count: u32,
    max_frame_latency: u32,
    present_mode: PresentMode,
    hdr: Option<HdrMetadata>,

    // The current `VkSwapchainKHR`'s.
    /// The surface's present modes.
    supported: Vec<VkPresentModeKHR>,
    /// Modes it presents with: the one it was created with first.
    modes: Vec<VkPresentModeKHR>,
    mode: VkPresentModeKHR,
    present_wait: bool,
    /// `frames` when it was created: present id `k` is frame `base + k`.
    base: u64,
    images: Vec<VkImage>,
    /// Images moved to `PRESENT_SRC_KHR` by their first acquire.
    initialized: Vec<bool>,
    present_semaphores: Vec<VkSemaphore>,
    /// The first-use transitions' signals, waited on by the app's next submit.
    init_semaphores: Vec<VkSemaphore>,
    /// Presenting failed with `OUT_OF_DATE`: recreate at the next acquire.
    stale: bool,
    displayed: Mutex<Displayed>,

    // For the swapchain's whole life.
    /// The first-use transitions' command buffers; reset with each `VkSwapchainKHR`.
    pool: VkCommandPool,
    /// Signalled with the frame number by each present's submission.
    timeline: VkSemaphore,
    /// Presents submitted.
    frames: u64,
    /// Free acquire semaphores, each with the frame whose completion frees it.
    acquire_semaphores: Vec<(VkSemaphore, u64)>,
    acquired: Option<Acquired>,
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
        let shared = &device.shared;
        shared.lost.check()?;
        let vk_format = surface_format(desc.format);
        if desc.width == 0
            || desc.height == 0
            || !(2..=16).contains(&desc.image_count)
            || vk_format.is_none()
        {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        if !shared.ext.swapchain || shared.fns.vkCreateSwapchainKHR.is_none() {
            return Err(unsupported());
        }
        let SurfaceHandle::Win32 { hwnd, .. } = surface;
        // SAFETY: the caller guarantees the window.
        let surface = unsafe { Surface::new(shared.instance.clone(), surface)? };
        if !surface.supports_family(shared.physical, queue.family) {
            return Err(unsupported());
        }
        let timeline = create_semaphore(shared, Some(0))?;
        let pool_info = VkCommandPoolCreateInfo {
            flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
            queueFamilyIndex: queue.family,
            ..Default::default()
        };
        let mut pool = 0;
        // SAFETY: valid create info and out pointer.
        let r = unsafe {
            (shared.fns.vkCreateCommandPool)(shared.raw, &pool_info, ptr::null(), &mut pool)
        };
        if let Err(e) = shared.lost.result(r) {
            // SAFETY: nothing uses the new semaphore.
            unsafe { (shared.fns.vkDestroySemaphore)(shared.raw, timeline, ptr::null()) };
            return Err(e);
        }
        let mut swapchain = Self {
            shared: shared.clone(),
            queue: queue.handle(),
            hwnd: hwnd.as_ptr() as usize,
            surface,
            raw: 0,
            features: SwapFeatures::query(shared),
            width: desc.width,
            height: desc.height,
            format: desc.format,
            vk_format: vk_format.unwrap_or(VK_FORMAT_UNDEFINED),
            color_space: desc.color_space,
            image_count: desc.image_count,
            max_frame_latency: desc.max_frame_latency.clamp(1, 16),
            present_mode: desc.present_mode,
            hdr: None,
            supported: Vec::new(),
            modes: Vec::new(),
            mode: VK_PRESENT_MODE_FIFO_KHR,
            present_wait: false,
            base: 0,
            images: Vec::new(),
            initialized: Vec::new(),
            present_semaphores: Vec::new(),
            init_semaphores: Vec::new(),
            stale: false,
            displayed: Mutex::new(Displayed::default()),
            pool,
            timeline,
            frames: 0,
            acquire_semaphores: Vec::new(),
            acquired: None,
            textures: Vec::new(),
            views: Vec::new(),
        };
        swapchain.create_chain()?;
        Ok(swapchain)
    }

    pub(crate) fn raw(&self) -> VkSwapchainKHR {
        self.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    /// Creates the `VkSwapchainKHR` (retiring the current one; the queue must be idle) and wraps
    /// its images. On failure the swapchain has no images (and no `VkSwapchainKHR` if creation
    /// failed) until the next successful call.
    fn create_chain(&mut self) -> Result<()> {
        let shared = self.shared.clone();
        let s = &*shared;
        let physical = s.physical;
        self.release_images();

        let supported = self.surface.present_modes(physical);
        let mode = resolve(self.present_mode, &supported);
        let caps = self.surface.caps(physical, mode, &self.features)?;
        let color_space = vk_color_space(self.color_space);
        if !self
            .surface
            .formats(physical)
            .iter()
            .any(|f| f.format == self.vk_format && f.colorSpace == color_space)
        {
            return Err(unsupported());
        }

        // The modes this swapchain can switch to per present, and an image count all allow.
        let c = &caps.caps;
        let mut modes = vec![mode];
        let (mut min_count, mut max_count) = (c.minImageCount, c.maxImageCount);
        if self.features.maintenance1 {
            for m in [
                VK_PRESENT_MODE_FIFO_KHR,
                VK_PRESENT_MODE_MAILBOX_KHR,
                VK_PRESENT_MODE_IMMEDIATE_KHR,
            ] {
                if m == mode || !supported.contains(&m) || !caps.compatible.contains(&m) {
                    continue;
                }
                let other = self.surface.caps(physical, m, &self.features)?.caps;
                min_count = min_count.max(other.minImageCount);
                max_count = match (max_count, other.maxImageCount) {
                    (0, n) | (n, 0) => n,
                    (a, b) => a.min(b),
                };
                modes.push(m);
            }
        }
        let mut count = self.image_count.max(min_count);
        if max_count != 0 {
            count = count.min(max_count);
        }

        let want = VkExtent2D {
            width: self.width,
            height: self.height,
        };
        let within = |lo: VkExtent2D, hi: VkExtent2D| {
            (lo.width..=hi.width).contains(&want.width)
                && (lo.height..=hi.height).contains(&want.height)
        };
        let (extent, scaled) = match caps.scaled {
            Some((lo, hi)) if within(lo, hi) => (want, true),
            _ if c.currentExtent.width != u32::MAX => (c.currentExtent, false),
            _ => (
                VkExtent2D {
                    width: want
                        .width
                        .clamp(c.minImageExtent.width, c.maxImageExtent.width),
                    height: want
                        .height
                        .clamp(c.minImageExtent.height, c.maxImageExtent.height),
                },
                false,
            ),
        };
        if extent.width == 0 || extent.height == 0 {
            // A minimized window takes no swapchain without scaling.
            return Err(Error::from_kind(ErrorKind::Other));
        }

        let gravity = match caps.gravity {
            true => VK_PRESENT_GRAVITY_MIN_BIT_KHR,
            false => 0,
        };
        let scaling = VkSwapchainPresentScalingCreateInfoKHR {
            scalingBehavior: VK_PRESENT_SCALING_STRETCH_BIT_KHR,
            presentGravityX: gravity,
            presentGravityY: gravity,
            ..Default::default()
        };
        let modes_info = VkSwapchainPresentModesCreateInfoKHR {
            pNext: match scaled {
                true => (&raw const scaling).cast(),
                false => ptr::null(),
            },
            presentModeCount: modes.len() as u32,
            pPresentModes: modes.as_ptr(),
            ..Default::default()
        };
        let next: *const core::ffi::c_void = match (self.features.maintenance1, scaled) {
            (true, _) => (&raw const modes_info).cast(),
            (false, true) => (&raw const scaling).cast(),
            (false, false) => ptr::null(),
        };
        let present_wait = caps.present_wait;
        let alpha = match c.supportedCompositeAlpha & VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR {
            0 => c.supportedCompositeAlpha & c.supportedCompositeAlpha.wrapping_neg(),
            _ => VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        };
        let (sharing, family_count, families) = s.families.sharing();
        let info = VkSwapchainCreateInfoKHR {
            pNext: next,
            flags: match present_wait {
                true => {
                    VK_SWAPCHAIN_CREATE_PRESENT_ID_2_BIT_KHR
                        | VK_SWAPCHAIN_CREATE_PRESENT_WAIT_2_BIT_KHR
                }
                false => 0,
            },
            surface: self.surface.raw,
            minImageCount: count,
            imageFormat: self.vk_format,
            imageColorSpace: color_space,
            imageExtent: extent,
            imageArrayLayers: 1,
            imageUsage: VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT
                | VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                | VK_IMAGE_USAGE_TRANSFER_DST_BIT,
            imageSharingMode: sharing,
            queueFamilyIndexCount: family_count,
            pQueueFamilyIndices: families,
            preTransform: c.currentTransform,
            compositeAlpha: alpha as VkCompositeAlphaFlagBitsKHR,
            presentMode: mode,
            clipped: VK_TRUE,
            oldSwapchain: self.raw,
            ..Default::default()
        };
        let create = s.fns.vkCreateSwapchainKHR.ok_or_else(unsupported)?;
        let mut raw = 0;
        // SAFETY: a valid create info chain (the surface supports what it asks) and out pointer.
        let r = unsafe { create(s.raw, &info, ptr::null(), &mut raw) };
        // The old swapchain is retired whether or not creation succeeded; the queue is idle.
        self.destroy_raw();
        s.lost.result(r)?;
        self.raw = raw;
        self.width = extent.width;
        self.height = extent.height;
        self.mode = mode;
        self.supported = supported;
        self.modes = modes;
        self.present_wait = present_wait;
        self.base = self.frames;
        self.stale = false;
        *self.displayed.lock() = Displayed::default();

        let get = s.fns.vkGetSwapchainImagesKHR.ok_or_else(unsupported)?;
        // SAFETY: the two-call idiom on a live swapchain.
        self.images = unsafe { enumerate(|n, p| get(s.raw, raw, n, p)) };
        self.initialized = vec![false; self.images.len()];
        for _ in 0..self.images.len() {
            let semaphore = create_semaphore(s, None)?;
            self.present_semaphores.push(semaphore);
        }
        if let Some(hdr) = self.hdr {
            self.apply_hdr_metadata(Some(&hdr));
        }
        if let Err(e) = self.wrap_images() {
            self.textures.clear();
            self.views.clear();
            return Err(e);
        }
        Ok(())
    }

    fn wrap_images(&mut self) -> Result<()> {
        let desc = TextureDesc::new_2d(
            self.width,
            self.height,
            self.format,
            TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
        );
        for &image in &self.images {
            let texture = RawTexture::from_image(&self.shared, image, &desc, false);
            let view = resource::render_target_view(
                &self.shared,
                &texture,
                &desc,
                &RenderTargetViewDesc::default(),
            )?;
            self.textures
                .push(crate::Texture::swapchain_image(texture, desc));
            self.views.push(crate::RenderTargetView { raw: view });
        }
        Ok(())
    }

    /// Drops the image wrappers and the current `VkSwapchainKHR`'s semaphores; the queue must be
    /// idle.
    fn release_images(&mut self) {
        let s = &*self.shared;
        self.views.clear();
        self.textures.clear();
        for semaphore in self
            .present_semaphores
            .drain(..)
            .chain(self.init_semaphores.drain(..))
        {
            // SAFETY: the queue is idle, so no present or submission waits on it.
            unsafe { (s.fns.vkDestroySemaphore)(s.raw, semaphore, ptr::null()) };
        }
        // SAFETY: the queue is idle, so the first-use transitions are done.
        unsafe { (s.fns.vkResetCommandPool)(s.raw, self.pool, 0) };
        self.images.clear();
        self.initialized.clear();
    }

    fn destroy_raw(&mut self) {
        if self.raw != 0
            && let Some(destroy) = self.shared.fns.vkDestroySwapchainKHR
        {
            // SAFETY: the queue is idle and nothing wraps its images any more.
            unsafe { destroy(self.shared.raw, self.raw, ptr::null()) };
        }
        self.raw = 0;
    }

    /// Waits until the queue is done with the swapchain: takes the acquired image's semaphore off
    /// the pending waits (waiting on it in an empty submission), then waits for the queue to idle.
    fn wait_idle(&mut self) -> Result<()> {
        let s = &*self.shared;
        let acquired = self.acquired.take();
        let r = self.queue.with_lock(|queue, state| {
            if let Some(a) = &acquired
                && let Some(i) = state.pending_waits.iter().position(|&w| w == a.wait)
            {
                state.pending_waits.swap_remove(i);
                let wait = semaphore_info(a.wait, 0);
                let submit = VkSubmitInfo2 {
                    waitSemaphoreInfoCount: 1,
                    pWaitSemaphoreInfos: &wait,
                    ..Default::default()
                };
                // SAFETY: the queue is locked; the semaphore has a pending signal.
                let r = unsafe { (s.fns.vkQueueSubmit2)(queue, 1, &submit, 0) };
                if r < 0 {
                    return r;
                }
            }
            // SAFETY: the queue is locked.
            unsafe { (s.fns.vkQueueWaitIdle)(queue) }
        });
        if let Some(a) = acquired {
            self.acquire_semaphores.push((a.semaphore, 0));
        }
        s.lost.result(r)
    }

    /// Waits for idle and recreates the `VkSwapchainKHR`.
    fn recreate(&mut self) -> Result<()> {
        self.wait_idle()?;
        self.create_chain()
    }

    /// Blocks until at most `max_frame_latency` frames are queued.
    fn pace(&self, timeout: Option<Duration>) -> Result<()> {
        let s = &*self.shared;
        let next = self.frames + 1;
        let latency = u64::from(self.max_frame_latency);
        if next <= latency {
            return Ok(());
        }
        let frame = next - latency;
        if self.present_wait
            && frame > self.base
            && let Some(wait) = s.fns.vkWaitForPresent2KHR
        {
            let id = frame - self.base;
            let info = VkPresentWait2InfoKHR {
                presentId: id,
                timeout: timeout_ns(timeout),
                ..Default::default()
            };
            // SAFETY: a live swapchain created with present waits; `id` was presented.
            match unsafe { wait(s.raw, self.raw, &info) } {
                VK_SUCCESS | VK_SUBOPTIMAL_KHR => {
                    self.saw_displayed(id);
                    return Ok(());
                }
                VK_TIMEOUT => return Err(timed_out()),
                // The present never reaches the display; the GPU's progress paces instead.
                VK_ERROR_OUT_OF_DATE_KHR | VK_ERROR_SURFACE_LOST_KHR => {}
                r => return Err(s.lost.result(r).err().unwrap_or_else(|| error(r))),
            }
        }
        let info = VkSemaphoreWaitInfo {
            semaphoreCount: 1,
            pSemaphores: &self.timeline,
            pValues: &frame,
            ..Default::default()
        };
        // SAFETY: a live timeline semaphore.
        match unsafe { (s.fns.vkWaitSemaphores)(s.raw, &info, timeout_ns(timeout)) } {
            VK_SUCCESS => Ok(()),
            VK_TIMEOUT => Err(timed_out()),
            r => Err(s.lost.result(r).err().unwrap_or_else(|| error(r))),
        }
    }

    /// Records that present `id` reached the display, with the current vertical blank.
    fn saw_displayed(&self, id: u64) {
        let mut displayed = self.displayed.lock();
        if id > displayed.id {
            displayed.id = id;
            displayed.refresh = refresh_count().map_or(0, |(count, _)| count);
        }
    }

    /// An acquire semaphore whose last use is done.
    fn take_acquire_semaphore(&mut self) -> Result<VkSemaphore> {
        let s = &*self.shared;
        let mut done = 0;
        // SAFETY: a live timeline semaphore and out pointer.
        let r = unsafe { (s.fns.vkGetSemaphoreCounterValue)(s.raw, self.timeline, &mut done) };
        s.lost.result(r)?;
        match self
            .acquire_semaphores
            .iter()
            .position(|&(_, frame)| frame <= done)
        {
            Some(i) => Ok(self.acquire_semaphores.swap_remove(i).0),
            None => create_semaphore(s, None),
        }
    }

    pub(crate) fn acquire(&mut self, timeout: Option<Duration>) -> Result<u32> {
        self.shared.lost.check()?;
        if self.acquired.is_some() {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let wants_recreate = !self.modes.contains(&self.wanted_mode());
        if self.raw == 0 || self.stale || self.textures.is_empty() || wants_recreate {
            self.recreate()?;
        }
        self.pace(timeout)?;
        let semaphore = self.take_acquire_semaphore()?;
        let mut index = 0;
        let mut r = self.acquire_raw(semaphore, timeout, &mut index);
        if r == VK_ERROR_OUT_OF_DATE_KHR {
            if let Err(e) = self.recreate() {
                self.acquire_semaphores.push((semaphore, 0));
                return Err(e);
            }
            r = self.acquire_raw(semaphore, timeout, &mut index);
        }
        match r {
            VK_SUCCESS | VK_SUBOPTIMAL_KHR => {}
            _ => {
                // Not signalled: free for the next try.
                self.acquire_semaphores.push((semaphore, 0));
                return match r {
                    VK_TIMEOUT | VK_NOT_READY => Err(timed_out()),
                    r => Err(self.shared.lost.result(r).err().unwrap_or_else(|| error(r))),
                };
            }
        }
        let wait = match self.initialized[index as usize] {
            true => {
                self.queue
                    .with_lock(|_, state| state.pending_waits.push(semaphore));
                semaphore
            }
            false => match self.initialize(index, semaphore) {
                Ok(wait) => wait,
                Err(e) => {
                    // The acquire semaphore stays signalled; let the next submit consume it.
                    self.queue
                        .with_lock(|_, state| state.pending_waits.push(semaphore));
                    self.acquired = Some(Acquired {
                        index,
                        semaphore,
                        wait: semaphore,
                    });
                    return Err(e);
                }
            },
        };
        self.acquired = Some(Acquired {
            index,
            semaphore,
            wait,
        });
        Ok(index)
    }

    fn acquire_raw(
        &self,
        semaphore: VkSemaphore,
        timeout: Option<Duration>,
        index: &mut u32,
    ) -> VkResult {
        let s = &*self.shared;
        let Some(acquire) = s.fns.vkAcquireNextImageKHR else {
            return VK_ERROR_EXTENSION_NOT_PRESENT;
        };
        // SAFETY: a live swapchain, an unsignalled semaphore with no pending operations.
        unsafe { acquire(s.raw, self.raw, timeout_ns(timeout), semaphore, 0, index) }
    }

    /// Moves image `index` from `UNDEFINED` to `PRESENT_SRC_KHR` after its acquire; returns the
    /// semaphore the app's next submit waits on instead of the acquire's.
    fn initialize(&mut self, index: u32, acquired: VkSemaphore) -> Result<VkSemaphore> {
        let s = &*self.shared;
        let f = &s.fns;
        let alloc = VkCommandBufferAllocateInfo {
            commandPool: self.pool,
            level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: 1,
            ..Default::default()
        };
        let mut cmd = ptr::null_mut();
        // SAFETY: the pool is only used here, from `&mut self`.
        let r = unsafe { (f.vkAllocateCommandBuffers)(s.raw, &alloc, &mut cmd) };
        s.lost.result(r)?;
        let begin = VkCommandBufferBeginInfo {
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            ..Default::default()
        };
        let barrier = VkImageMemoryBarrier2 {
            srcStageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            dstStageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            oldLayout: VK_IMAGE_LAYOUT_UNDEFINED,
            newLayout: VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
            dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
            image: self.images[index as usize],
            subresourceRange: VkImageSubresourceRange {
                aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                baseMipLevel: 0,
                levelCount: 1,
                baseArrayLayer: 0,
                layerCount: 1,
            },
            ..Default::default()
        };
        let dependency = VkDependencyInfo {
            imageMemoryBarrierCount: 1,
            pImageMemoryBarriers: &barrier,
            ..Default::default()
        };
        // SAFETY: a new command buffer, recorded once; the image is acquired.
        unsafe {
            s.lost.result((f.vkBeginCommandBuffer)(cmd, &begin))?;
            (f.vkCmdPipelineBarrier2)(cmd, &dependency);
            s.lost.result((f.vkEndCommandBuffer)(cmd))?;
        }
        let signal = create_semaphore(s, None)?;
        self.init_semaphores.push(signal);
        let wait = semaphore_info(acquired, 0);
        let signal_info = semaphore_info(signal, 0);
        let list = VkCommandBufferSubmitInfo {
            commandBuffer: cmd,
            ..Default::default()
        };
        let submit = VkSubmitInfo2 {
            waitSemaphoreInfoCount: 1,
            pWaitSemaphoreInfos: &wait,
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &list,
            signalSemaphoreInfoCount: 1,
            pSignalSemaphoreInfos: &signal_info,
            ..Default::default()
        };
        let r = self.queue.with_lock(|queue, state| {
            // SAFETY: the queue is locked; the command buffer is ended and of its family.
            let r = unsafe { (f.vkQueueSubmit2)(queue, 1, &submit, 0) };
            if r >= 0 {
                state.pending_waits.push(signal);
            }
            r
        });
        s.lost.result(r)?;
        self.initialized[index as usize] = true;
        Ok(signal)
    }

    /// The mode `present_mode` resolves to on this surface.
    fn wanted_mode(&self) -> VkPresentModeKHR {
        resolve(self.present_mode, &self.supported)
    }

    pub(crate) fn present(&mut self) -> Result<PresentStatus> {
        let shared = self.shared.clone();
        let s = &*shared;
        s.lost.check()?;
        let present = s.fns.vkQueuePresentKHR.ok_or_else(unsupported)?;
        let Some(acquired) = self.acquired.take() else {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        };
        let index = acquired.index;
        let frame = self.frames + 1;
        let present_semaphore = self.present_semaphores[index as usize];
        // A per-present mode switch, among the modes the swapchain was created with.
        let mode =
            Some(self.wanted_mode()).filter(|m| self.modes.len() > 1 && self.modes.contains(m));
        let present_id = frame - self.base;
        let ids = VkPresentId2KHR {
            swapchainCount: 1,
            pPresentIds: &present_id,
            ..Default::default()
        };
        let ids_ptr: *const core::ffi::c_void = match self.present_wait {
            true => (&raw const ids).cast(),
            false => ptr::null(),
        };
        let mode_value = mode.unwrap_or(self.mode);
        let mode_info = VkSwapchainPresentModeInfoKHR {
            pNext: ids_ptr,
            swapchainCount: 1,
            pPresentModes: &mode_value,
            ..Default::default()
        };
        let info = VkPresentInfoKHR {
            pNext: match mode {
                Some(_) => (&raw const mode_info).cast(),
                None => ids_ptr,
            },
            waitSemaphoreCount: 1,
            pWaitSemaphores: &present_semaphore,
            swapchainCount: 1,
            pSwapchains: &self.raw,
            pImageIndices: &index,
            ..Default::default()
        };
        let timeline = self.timeline;
        let (submitted, r) = self.queue.with_lock(|queue, state| {
            // The acquire's wait, unless a submit since took it.
            let mut waits = Vec::new();
            if let Some(i) = state.pending_waits.iter().position(|&w| w == acquired.wait) {
                state.pending_waits.swap_remove(i);
                waits.push(semaphore_info(acquired.wait, 0));
            }
            let signals = [
                semaphore_info(present_semaphore, 0),
                semaphore_info(timeline, frame),
            ];
            let submit = VkSubmitInfo2 {
                waitSemaphoreInfoCount: waits.len() as u32,
                pWaitSemaphoreInfos: waits.as_ptr(),
                signalSemaphoreInfoCount: signals.len() as u32,
                pSignalSemaphoreInfos: signals.as_ptr(),
                ..Default::default()
            };
            // SAFETY: the queue is locked; the present semaphore is unsignalled (the image's last
            // present is done: it was acquired again).
            let r = unsafe { (s.fns.vkQueueSubmit2)(queue, 1, &submit, 0) };
            if r < 0 {
                return (false, r);
            }
            // SAFETY: the queue is locked; the image is acquired; valid chains.
            (true, unsafe { present(queue, &info) })
        });
        // The acquire semaphore is free once this frame's submission is done.
        self.acquire_semaphores.push((acquired.semaphore, frame));
        if !submitted {
            return Err(s.lost.result(r).err().unwrap_or_else(|| error(r)));
        }
        self.frames = frame;
        if let Some(m) = mode {
            self.mode = m;
        }
        match r {
            VK_SUCCESS | VK_SUBOPTIMAL_KHR if self.minimized() => Ok(PresentStatus::Occluded),
            VK_SUCCESS | VK_SUBOPTIMAL_KHR => Ok(PresentStatus::Presented),
            VK_ERROR_OUT_OF_DATE_KHR => {
                // Not shown; the next acquire recreates the swapchain.
                self.stale = true;
                Ok(PresentStatus::Occluded)
            }
            r => Err(s.lost.result(r).err().unwrap_or_else(|| error(r))),
        }
    }

    fn minimized(&self) -> bool {
        #[cfg(marlin_platform = "windows")]
        {
            win32::minimized(self.hwnd)
        }
        #[cfg(not(marlin_platform = "windows"))]
        {
            false
        }
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let whole = self.raw != 0 && !self.stale && !self.textures.is_empty();
        if (width, height) == (self.width, self.height) && whole {
            return Ok(());
        }
        self.wait_idle()?;
        self.width = width;
        self.height = height;
        self.create_chain()
    }

    pub(crate) fn set_present_mode(&mut self, mode: PresentMode) {
        // Modes the swapchain can switch to apply at the next present; others recreate it at the
        // next acquire.
        self.present_mode = mode;
    }

    pub(crate) fn supports_color_space(&self, color_space: ColorSpace) -> bool {
        if color_space != ColorSpace::Srgb && !self.shared.instance.ext.swapchain_colorspace {
            return false;
        }
        let wanted = vk_color_space(color_space);
        self.surface
            .formats(self.shared.physical)
            .iter()
            .any(|f| f.format == self.vk_format && f.colorSpace == wanted)
    }

    pub(crate) fn set_color_space(&mut self, color_space: ColorSpace) -> Result<()> {
        if color_space == self.color_space && self.raw != 0 {
            return Ok(());
        }
        let old = self.color_space;
        self.color_space = color_space;
        if let Err(e) = self.recreate() {
            self.color_space = old;
            let _ = self.create_chain();
            return Err(e);
        }
        Ok(())
    }

    pub(crate) fn set_hdr_metadata(&mut self, metadata: Option<&HdrMetadata>) -> Result<()> {
        if !self.shared.ext.hdr_metadata || self.shared.fns.vkSetHdrMetadataEXT.is_none() {
            return Err(unsupported());
        }
        self.hdr = metadata.copied();
        self.apply_hdr_metadata(metadata);
        Ok(())
    }

    /// Sends `metadata` to the display; `None` sends zeros, which HDR10 static metadata
    /// (CTA-861.3) reads as unknown.
    fn apply_hdr_metadata(&self, metadata: Option<&HdrMetadata>) {
        let s = &*self.shared;
        let Some(set) = s.fns.vkSetHdrMetadataEXT.filter(|_| s.ext.hdr_metadata) else {
            return;
        };
        if self.raw == 0 {
            return;
        }
        let xy = |c: [f32; 2]| VkXYColorEXT { x: c[0], y: c[1] };
        let data = match metadata {
            Some(m) => VkHdrMetadataEXT {
                displayPrimaryRed: xy(m.red_primary),
                displayPrimaryGreen: xy(m.green_primary),
                displayPrimaryBlue: xy(m.blue_primary),
                whitePoint: xy(m.white_point),
                maxLuminance: m.max_mastering_luminance,
                minLuminance: m.min_mastering_luminance,
                maxContentLightLevel: m.max_content_light_level,
                maxFrameAverageLightLevel: m.max_frame_average_light_level,
                ..Default::default()
            },
            None => VkHdrMetadataEXT::default(),
        };
        // SAFETY: a live swapchain and valid metadata.
        unsafe { set(s.raw, 1, &self.raw, &data) };
    }

    pub(crate) fn containing_output(&self) -> Result<OutputInfo> {
        #[cfg(marlin_platform = "windows")]
        {
            win32::containing(self.hwnd).ok_or_else(unsupported)
        }
        #[cfg(not(marlin_platform = "windows"))]
        {
            Err(unsupported())
        }
    }

    /// From present waits: the newest present seen done, and the display's vertical blank count
    /// when the RHI saw it (an upper bound of when it was shown). `None` without present waits.
    pub(crate) fn frame_statistics(&self) -> Result<Option<FrameStatistics>> {
        let s = &*self.shared;
        s.lost.check()?;
        let Some(wait) = s
            .fns
            .vkWaitForPresent2KHR
            .filter(|_| self.present_wait && self.raw != 0)
        else {
            return Ok(None);
        };
        let presented = self.frames - self.base;
        let seen = self.displayed.lock().id;
        // A wait for id `k` returns once a present with id `k` or later is done: newest first,
        // over the frames that may still be queued.
        for id in (seen + 1..=presented).rev().take(16) {
            let info = VkPresentWait2InfoKHR {
                presentId: id,
                timeout: 0,
                ..Default::default()
            };
            // SAFETY: a live swapchain created with present waits; `id` was presented.
            match unsafe { wait(s.raw, self.raw, &info) } {
                VK_SUCCESS | VK_SUBOPTIMAL_KHR => {
                    self.saw_displayed(id);
                    break;
                }
                VK_TIMEOUT => {}
                _ => break,
            }
        }
        let displayed = self.displayed.lock();
        if displayed.id == 0 {
            return Ok(None);
        }
        let (sync_refresh, sync_time) = refresh_count().unwrap_or((0, 0));
        Ok(Some(FrameStatistics {
            present_count: displayed.id as u32,
            present_refresh_count: displayed.refresh as u32,
            sync_refresh_count: sync_refresh as u32,
            sync_cpu_time: sync_time,
        }))
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn format(&self) -> Format {
        self.format
    }

    pub(crate) fn color_space(&self) -> ColorSpace {
        self.color_space
    }

    pub(crate) fn textures(&self) -> &[crate::Texture] {
        &self.textures
    }

    pub(crate) fn views(&self) -> &[crate::RenderTargetView] {
        &self.views
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        // The GPU and the presentation engine may still use the images and semaphores.
        let _ = self.wait_idle();
        self.release_images();
        self.destroy_raw();
        let s = &*self.shared;
        // SAFETY: the queue is idle; nothing uses these any more.
        unsafe {
            for (semaphore, _) in self.acquire_semaphores.drain(..) {
                (s.fns.vkDestroySemaphore)(s.raw, semaphore, ptr::null());
            }
            (s.fns.vkDestroySemaphore)(s.raw, self.timeline, ptr::null());
            (s.fns.vkDestroyCommandPool)(s.raw, self.pool, ptr::null());
        }
        // The surface goes last, with its field.
    }
}

/// A binary semaphore, or a timeline one starting at `timeline`.
fn create_semaphore(shared: &Shared, timeline: Option<u64>) -> Result<VkSemaphore> {
    let ty = VkSemaphoreTypeCreateInfo {
        semaphoreType: VK_SEMAPHORE_TYPE_TIMELINE,
        initialValue: timeline.unwrap_or(0),
        ..Default::default()
    };
    let info = VkSemaphoreCreateInfo {
        pNext: match timeline {
            Some(_) => (&raw const ty).cast(),
            None => ptr::null(),
        },
        ..Default::default()
    };
    let mut raw = 0;
    // SAFETY: a valid create info chain and out pointer.
    let r = unsafe { (shared.fns.vkCreateSemaphore)(shared.raw, &info, ptr::null(), &mut raw) };
    shared.lost.result(r)?;
    Ok(raw)
}

/// A semaphore wait or signal covering all commands.
fn semaphore_info(semaphore: VkSemaphore, value: u64) -> VkSemaphoreSubmitInfo {
    VkSemaphoreSubmitInfo {
        semaphore,
        value,
        stageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
        ..Default::default()
    }
}

/// The display's vertical blank count and its time in performance-counter ticks.
fn refresh_count() -> Option<(u64, u64)> {
    #[cfg(marlin_platform = "windows")]
    {
        win32::refresh()
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        None
    }
}

#[cfg(marlin_platform = "windows")]
mod win32 {
    //! Displays through DXGI (HDR state and luminance, matched to Vulkan adapters by LUID) and
    //! GDI (the window's monitor), vertical blanks from DWM, and the window `tearing` probes
    //! with. windows-sys has no DXGI: its few calls are bound here by vtable slot.

    use core::ffi::c_void;
    use core::ptr::{self, NonNull};

    use marlin_pal::ext::dylib::Dylib;
    use marlin_sync::OnceLock;
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, IsIconic, WS_POPUP,
    };

    use crate::{ColorSpace, OutputInfo, Rect, SurfaceHandle};

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);

    const IID_IDXGI_FACTORY1: Guid = Guid(
        0x770aae78,
        0xf26f,
        0x4dba,
        [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
    );
    const IID_IDXGI_OUTPUT6: Guid = Guid(
        0x068346e8,
        0xaaec,
        0x4b84,
        [0xad, 0xd7, 0x13, 0x7f, 0x51, 0x3f, 0x77, 0xa1],
    );

    // Vtable slots: IUnknown 0..3, IDXGIObject 3..7, then each interface's methods in order.
    const QUERY_INTERFACE: usize = 0;
    const RELEASE: usize = 2;
    /// IDXGIFactory 7..12, then IDXGIFactory1::EnumAdapters1.
    const ENUM_ADAPTERS1: usize = 12;
    /// IDXGIAdapter::EnumOutputs.
    const ENUM_OUTPUTS: usize = 7;
    /// IDXGIAdapter 7..10, then IDXGIAdapter1::GetDesc1.
    const ADAPTER_GET_DESC1: usize = 10;
    /// IDXGIOutput through IDXGIOutput5 7..27, then IDXGIOutput6::GetDesc1.
    const OUTPUT_GET_DESC1: usize = 27;

    const DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709: i32 = 1;
    const DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: i32 = 12;

    /// `DXGI_ADAPTER_DESC1`.
    #[repr(C)]
    struct AdapterDesc1 {
        description: [u16; 128],
        vendor_id: u32,
        device_id: u32,
        sub_sys_id: u32,
        revision: u32,
        dedicated_video_memory: usize,
        dedicated_system_memory: usize,
        shared_system_memory: usize,
        luid_low: u32,
        luid_high: i32,
        flags: u32,
    }

    /// `DXGI_OUTPUT_DESC1`.
    #[repr(C)]
    struct OutputDesc1 {
        device_name: [u16; 32],
        desktop: RECT,
        attached_to_desktop: i32,
        rotation: i32,
        monitor: *mut c_void,
        bits_per_color: u32,
        color_space: i32,
        red_primary: [f32; 2],
        green_primary: [f32; 2],
        blue_primary: [f32; 2],
        white_point: [f32; 2],
        min_luminance: f32,
        max_luminance: f32,
        max_full_frame_luminance: f32,
    }

    /// An owned COM interface pointer.
    struct Com(*mut c_void);

    type Enum = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;

    impl Com {
        /// The method in vtable slot `slot`.
        ///
        /// # Safety
        /// `F` is that method's type.
        unsafe fn method<F: Copy>(&self, slot: usize) -> F {
            // SAFETY: a live interface starts with its vtable pointer; the caller vouches for
            // the slot's type.
            unsafe {
                let vtbl = *self.0.cast::<*const *const c_void>();
                core::mem::transmute_copy(&*vtbl.add(slot))
            }
        }

        fn query(&self, iid: &Guid) -> Option<Com> {
            type F = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
            let mut out = ptr::null_mut();
            // SAFETY: IUnknown::QueryInterface with a valid IID and out pointer.
            let hr = unsafe { self.method::<F>(QUERY_INTERFACE)(self.0, iid, &mut out) };
            (hr >= 0 && !out.is_null()).then(|| Com(out))
        }

        /// Items `0..` of an `Enum*(index, out)` method, until it fails.
        fn enumerate(&self, slot: usize) -> impl Iterator<Item = Com> + '_ {
            (0..).map_while(move |i| {
                let mut out = ptr::null_mut();
                // SAFETY: `slot` is an enumeration method of this interface; valid out pointer.
                let hr = unsafe { self.method::<Enum>(slot)(self.0, i, &mut out) };
                (hr >= 0 && !out.is_null()).then(|| Com(out))
            })
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            type F = unsafe extern "system" fn(*mut c_void) -> u32;
            // SAFETY: IUnknown::Release of the reference this owns.
            unsafe { self.method::<F>(RELEASE)(self.0) };
        }
    }

    type Create = unsafe extern "system" fn(*const Guid, *mut *mut c_void) -> i32;

    /// `CreateDXGIFactory1`. dxgi.dll stays loaded for the process: unloading it while its
    /// threads run crashes.
    fn create_factory() -> Option<Create> {
        static CREATE: OnceLock<Option<Create>> = OnceLock::new();
        *CREATE.get_or_init(|| {
            let library = Dylib::get()?.open("dxgi.dll").ok()?;
            // SAFETY: dxgi.dll exports CreateDXGIFactory1 with this signature.
            let create = unsafe { library.symbol::<Create>(c"CreateDXGIFactory1") };
            core::mem::forget(library);
            create
        })
    }

    /// A DXGI factory.
    struct Dxgi {
        factory: Com,
    }

    impl Dxgi {
        fn new() -> Option<Self> {
            let create = create_factory()?;
            let mut factory = ptr::null_mut();
            // SAFETY: a valid IID and out pointer.
            if unsafe { create(&IID_IDXGI_FACTORY1, &mut factory) } < 0 || factory.is_null() {
                return None;
            }
            Some(Self {
                factory: Com(factory),
            })
        }

        fn adapters(&self) -> impl Iterator<Item = Com> + '_ {
            self.factory.enumerate(ENUM_ADAPTERS1)
        }
    }

    fn luid(adapter: &Com) -> Option<u64> {
        type F = unsafe extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32;
        // SAFETY: all-zero is a valid DXGI_ADAPTER_DESC1.
        let mut desc: AdapterDesc1 = unsafe { core::mem::zeroed() };
        // SAFETY: IDXGIAdapter1::GetDesc1 with a valid out pointer.
        let hr = unsafe { adapter.method::<F>(ADAPTER_GET_DESC1)(adapter.0, &mut desc) };
        (hr >= 0).then_some((desc.luid_high as u32 as u64) << 32 | desc.luid_low as u64)
    }

    fn desc(output: &Com) -> Option<OutputDesc1> {
        let output6 = output.query(&IID_IDXGI_OUTPUT6)?;
        type F = unsafe extern "system" fn(*mut c_void, *mut OutputDesc1) -> i32;
        // SAFETY: all-zero is a valid DXGI_OUTPUT_DESC1.
        let mut desc: OutputDesc1 = unsafe { core::mem::zeroed() };
        // SAFETY: IDXGIOutput6::GetDesc1 with a valid out pointer.
        let hr = unsafe { output6.method::<F>(OUTPUT_GET_DESC1)(output6.0, &mut desc) };
        (hr >= 0).then_some(desc)
    }

    fn rect(r: RECT) -> Rect {
        Rect {
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
        }
    }

    /// A NUL-terminated UTF-16 name as UTF-8 in `buf`.
    fn name<'a>(s: &[u16], buf: &'a mut [u8]) -> &'a str {
        let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        let mut n = 0;
        for c in char::decode_utf16(s[..len].iter().copied()) {
            let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
            if n + c.len_utf8() > buf.len() {
                break;
            }
            n += c.encode_utf8(&mut buf[n..]).len();
        }
        core::str::from_utf8(&buf[..n]).unwrap_or_default()
    }

    fn info(d: &OutputDesc1) -> OutputInfo {
        let mut buf = [0; 64];
        let mut info = OutputInfo::new(name(&d.device_name, &mut buf), rect(d.desktop));
        info.bits_per_color = d.bits_per_color;
        info.color_space = match d.color_space {
            DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 => ColorSpace::Hdr10,
            DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 => ColorSpace::ScRgb,
            _ => ColorSpace::Srgb,
        };
        info.red_primary = d.red_primary;
        info.green_primary = d.green_primary;
        info.blue_primary = d.blue_primary;
        info.white_point = d.white_point;
        info.min_luminance = d.min_luminance;
        info.max_luminance = d.max_luminance;
        info.max_full_frame_luminance = d.max_full_frame_luminance;
        info
    }

    /// Display `index` of the adapter with `luid`.
    pub(super) fn output(luid: u64, index: u32) -> Option<OutputInfo> {
        let dxgi = Dxgi::new()?;
        let adapter = dxgi.adapters().find(|a| self::luid(a) == Some(luid))?;
        let output = adapter.enumerate(ENUM_OUTPUTS).nth(index as usize)?;
        desc(&output).map(|d| info(&d))
    }

    /// The display most of the window is on, from whichever adapter drives it.
    pub(super) fn containing(hwnd: usize) -> Option<OutputInfo> {
        // SAFETY: any window handle; a stale one yields the nearest monitor or null.
        let monitor = unsafe { MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_null() {
            return None;
        }
        if let Some(dxgi) = Dxgi::new() {
            for adapter in dxgi.adapters() {
                for output in adapter.enumerate(ENUM_OUTPUTS) {
                    if let Some(d) = desc(&output)
                        && d.monitor == monitor
                    {
                        return Some(info(&d));
                    }
                }
            }
        }
        // No DXGI output for it: GDI's name and rectangle.
        // SAFETY: all-zero is a valid MONITORINFOEXW.
        let mut mi: MONITORINFOEXW = unsafe { core::mem::zeroed() };
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: a monitor handle and a MONITORINFOEXW with its size set.
        if unsafe { GetMonitorInfoW(monitor, (&raw mut mi).cast()) } == 0 {
            return None;
        }
        let mut buf = [0; 64];
        Some(OutputInfo::new(
            name(&mi.szDevice, &mut buf),
            rect(mi.monitorInfo.rcMonitor),
        ))
    }

    pub(super) fn minimized(hwnd: usize) -> bool {
        // SAFETY: any window handle.
        unsafe { IsIconic(hwnd as HWND) != 0 }
    }

    /// DWM's vertical blank count and its QueryPerformanceCounter time.
    pub(super) fn refresh() -> Option<(u64, u64)> {
        // SAFETY: all-zero is a valid DWM_TIMING_INFO.
        let mut t: DWM_TIMING_INFO = unsafe { core::mem::zeroed() };
        t.cbSize = size_of::<DWM_TIMING_INFO>() as u32;
        // SAFETY: a null window (the desktop's timing) and a sized out structure.
        let hr = unsafe { DwmGetCompositionTimingInfo(ptr::null_mut(), &mut t) };
        (hr >= 0).then_some((t.cRefresh, t.qpcVBlank))
    }

    /// A hidden 1x1 window to make a surface on before any swapchain exists.
    pub(super) struct ProbeWindow {
        hwnd: HWND,
        hinstance: HINSTANCE,
    }

    impl ProbeWindow {
        pub(super) fn new() -> Option<Self> {
            let class = [b'S', b'T', b'A', b'T', b'I', b'C', 0].map(u16::from);
            // SAFETY: the module's own handle.
            let hinstance = unsafe { GetModuleHandleW(ptr::null()) };
            // SAFETY: a system window class and valid arguments; never shown.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    hinstance,
                    ptr::null(),
                )
            };
            (!hwnd.is_null() && !hinstance.is_null()).then_some(Self { hwnd, hinstance })
        }

        pub(super) fn handle(&self) -> SurfaceHandle {
            SurfaceHandle::Win32 {
                hwnd: NonNull::new(self.hwnd).expect("checked in new"),
                hinstance: NonNull::new(self.hinstance).expect("checked in new"),
            }
        }
    }

    impl Drop for ProbeWindow {
        fn drop(&mut self) {
            // SAFETY: the window this created, on the thread that created it.
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}
