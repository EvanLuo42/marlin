//! Flip-model DXGI swapchain with a frame-latency waitable object, HDR color spaces and
//! metadata, the containing output and presentation statistics.

use core::ffi::c_void;
use core::time::Duration;

use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::com::{self, *};
use super::diag::{self, Lost, Validation, flush};
use super::ffi::*;
use super::resource::{RenderTargetView as RawView, Texture as RawTexture, dxgi_format};
use super::{Backend, Device, Handle, Idle, Queue, check, error, output_info, wait_handle};
use crate::{
    ColorSpace, Error, ErrorKind, Format, FrameStatistics, HdrMetadata, OutputInfo, PresentMode,
    PresentStatus, RenderTargetView, Result, SurfaceHandle, SwapchainDesc, Texture, TextureDesc,
    TextureUsage,
};

pub(crate) struct Swapchain {
    swapchain: Com<IDXGISwapChain3>,
    /// HDR metadata (Windows 10 1709).
    swapchain4: Option<Com<IDXGISwapChain4>>,
    device: Com<ID3D12Device>,
    rtv_heap: Com<ID3D12DescriptorHeap>,
    rtv_size: u32,
    waitable: Handle,
    flags: u32,
    format: Format,
    color_space: ColorSpace,
    width: u32,
    height: u32,
    present_mode: PresentMode,
    tearing: bool,
    /// Images the swapchain has, whether or not `textures` wraps them (it is empty after a
    /// failed resize).
    image_count: u32,
    textures: Vec<Texture>,
    views: Vec<RenderTargetView>,
    idle: Idle,
    validation: Option<Arc<Validation>>,
    lost: Arc<Lost>,
}

/// Formats DXGI flip-model swapchains accept (sRGB ones through their views).
fn presentable(format: Format) -> bool {
    matches!(
        format.linear(),
        Format::Rgba8Unorm | Format::Bgra8Unorm | Format::Rgb10a2Unorm | Format::Rgba16Float
    )
}

fn dxgi_color_space(color_space: ColorSpace) -> i32 {
    match color_space {
        ColorSpace::Srgb => DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        ColorSpace::Hdr10 => DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        ColorSpace::ScRgb => DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709,
    }
}

impl Swapchain {
    /// # Safety
    /// `surface` is a live window that outlives the swapchain.
    pub(super) unsafe fn new(
        device: &Device,
        queue: &Queue,
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
    ) -> Result<Self> {
        let SurfaceHandle::Win32 { hwnd, .. } = surface;
        if desc.width == 0
            || desc.height == 0
            || !(2..=16).contains(&desc.image_count)
            || !presentable(desc.format)
        {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let tearing = device.features.tearing;
        let mut flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        if tearing {
            flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;
        }
        // Flip-model swapchains cannot have sRGB formats; the views add the encoding.
        let dxgi_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: desc.width,
            Height: desc.height,
            Format: dxgi_format(desc.format.linear()),
            Stereo: 0,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: desc.image_count,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: flags,
        };
        let factory = &device.factory.vtbl().base;
        let mut swapchain1 = core::ptr::null_mut();
        // SAFETY: valid factory and queue; the caller guarantees `hwnd`; `dxgi_desc` is complete.
        let hr = unsafe {
            (factory.CreateSwapChainForHwnd)(
                device.factory.as_raw(),
                queue.queue.as_raw(),
                hwnd.as_ptr(),
                &dxgi_desc,
                core::ptr::null(),
                core::ptr::null_mut(),
                &mut swapchain1,
            )
        };
        device.lost.result(hr)?;
        // SAFETY: success returned an owned IDXGISwapChain1.
        let swapchain1 = unsafe { Com::<IDXGISwapChain1>::from_raw(swapchain1) }
            .ok_or(Error::from_kind(ErrorKind::Other))?;
        let swapchain = swapchain1.cast::<IDXGISwapChain3>().map_err(error)?;

        // Alt+Enter would switch to exclusive fullscreen; the app does borderless itself.
        // SAFETY: valid factory and window.
        unsafe {
            (factory.MakeWindowAssociation)(
                device.factory.as_raw(),
                hwnd.as_ptr(),
                DXGI_MWA_NO_ALT_ENTER,
            )
        };

        let s = &swapchain;
        // SAFETY: valid interface, created with the waitable flag.
        check(unsafe {
            (s.vtbl().SetMaximumFrameLatency)(s.as_raw(), desc.max_frame_latency.clamp(1, 16))
        })?;
        // SAFETY: as above; the handle is ours to close.
        let waitable = Handle(unsafe { (s.vtbl().GetFrameLatencyWaitableObject)(s.as_raw()) });

        let heap_desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            NumDescriptors: desc.image_count,
            Flags: 0,
            NodeMask: 0,
        };
        let d = &device.device;
        // SAFETY: valid interface, description, IID and out pointer.
        let rtv_heap = com::create(|iid, out| unsafe {
            (d.vtbl().CreateDescriptorHeap)(d.as_raw(), &heap_desc, iid, out)
        })
        .map_err(error)?;

        let mut swapchain = Self {
            swapchain4: swapchain.cast().ok(),
            swapchain,
            device: device.device.clone(),
            rtv_heap,
            rtv_size: device.rtv_size,
            waitable,
            flags,
            format: desc.format,
            color_space: ColorSpace::Srgb,
            width: desc.width,
            height: desc.height,
            present_mode: desc.present_mode,
            tearing,
            image_count: desc.image_count,
            textures: Vec::with_capacity(desc.image_count as usize),
            views: Vec::with_capacity(desc.image_count as usize),
            idle: Idle::new(device, queue.queue.clone())?,
            validation: device.validation.clone(),
            lost: device.lost.clone(),
        };
        swapchain.create_views()?;
        if desc.color_space != ColorSpace::Srgb {
            if !<Backend as crate::contract::Swapchain>::supports_color_space(
                &swapchain,
                desc.color_space,
            ) {
                return Err(Error::from_kind(ErrorKind::Unsupported));
            }
            <Backend as crate::contract::Swapchain>::set_color_space(
                &mut swapchain,
                desc.color_space,
            )?;
        }
        Ok(swapchain)
    }

    pub(super) fn as_raw(&self) -> *mut c_void {
        self.swapchain.as_raw()
    }

    /// Wraps every image and creates its view; on failure leaves none, so the swapchain is
    /// either whole or image-less (and `resize` tries again).
    fn create_views(&mut self) -> Result<()> {
        self.textures.clear();
        self.views.clear();
        let result = self.wrap_images();
        if result.is_err() {
            self.textures.clear();
            self.views.clear();
        }
        result
    }

    fn wrap_images(&mut self) -> Result<()> {
        let count = self.image_count;
        let heap = &self.rtv_heap;
        let mut start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        // SAFETY: valid interface; the struct is returned through `start`.
        unsafe { (heap.vtbl().GetCPUDescriptorHandleForHeapStart)(heap.as_raw(), &mut start) };
        let view_desc = D3D12_RENDER_TARGET_VIEW_DESC {
            Format: dxgi_format(self.format),
            ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
            u: ViewUnion::default(),
        };
        for i in 0..count {
            let s = &self.swapchain;
            // SAFETY: valid interface, IID and out pointer; `i` < the buffer count.
            let resource = com::create::<ID3D12Resource>(|iid, out| unsafe {
                (s.vtbl().GetBuffer)(s.as_raw(), i, iid, out)
            })
            .map_err(error)?;
            // SAFETY: a live resource.
            unsafe { diag::set_name(resource.as_raw(), &format!("swapchain image {i}")) };
            let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: start.ptr + (i * self.rtv_size) as usize,
            };
            let d = &self.device;
            // SAFETY: valid device and resource; `handle` is inside the heap's `count` slots.
            unsafe {
                (d.vtbl().CreateRenderTargetView)(d.as_raw(), resource.as_raw(), &view_desc, handle)
            };
            let desc = TextureDesc::new_2d(
                self.width,
                self.height,
                self.format,
                TextureUsage::RENDER_TARGET | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
            );
            self.textures.push(Texture::swapchain_image(
                RawTexture::wrap(resource, &desc, dxgi_format(self.format.linear())),
                desc,
            ));
            self.views.push(RenderTargetView {
                raw: RawView {
                    handle,
                    _slot: None,
                },
            });
        }
        Ok(())
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        // The GPU may still be reading the images.
        let _ = self.idle.wait();
        flush(&self.validation);
    }
}

/// `x` rounded to the nearest integer in `0..=max` (core has no `f32::round`).
fn nearest(x: f32, max: f32) -> u32 {
    (x.clamp(0.0, max) + 0.5) as u32
}

/// CIE xy chromaticity in the 0.00002 units HDR10 metadata uses.
fn chromaticity(xy: [f32; 2]) -> [u16; 2] {
    xy.map(|c| nearest(c * 50000.0, 65535.0) as u16)
}

impl crate::contract::Swapchain for Backend {
    fn acquire(swapchain: &mut Swapchain, timeout: Option<Duration>) -> Result<u32> {
        if !wait_handle(swapchain.waitable.0, timeout)? {
            return Err(Error::from_kind(ErrorKind::TimedOut));
        }
        let s = &swapchain.swapchain;
        // SAFETY: valid interface.
        Ok(unsafe { (s.vtbl().GetCurrentBackBufferIndex)(s.as_raw()) })
    }

    fn present(swapchain: &mut Swapchain) -> Result<PresentStatus> {
        let (interval, flags) = match swapchain.present_mode {
            PresentMode::Fifo => (1, 0),
            PresentMode::Mailbox => (0, 0),
            PresentMode::Immediate if swapchain.tearing => (0, DXGI_PRESENT_ALLOW_TEARING),
            PresentMode::Immediate => (0, 0),
        };
        let s = &swapchain.swapchain;
        // SAFETY: valid interface; tearing is requested only if the swapchain allows it.
        let hr = unsafe { (s.vtbl().Present)(s.as_raw(), interval, flags) };
        flush(&swapchain.validation);
        match hr {
            DXGI_STATUS_OCCLUDED => Ok(PresentStatus::Occluded),
            hr => swapchain.lost.result(hr).map(|()| PresentStatus::Presented),
        }
    }

    fn resize(swapchain: &mut Swapchain, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        let whole = swapchain.textures.len() == swapchain.image_count as usize;
        if (width, height) == (swapchain.width, swapchain.height) && whole {
            return Ok(());
        }
        swapchain.idle.wait()?;
        // ResizeBuffers fails while any reference to the old buffers remains.
        swapchain.textures.clear();
        swapchain.views.clear();
        let s = &swapchain.swapchain;
        // SAFETY: valid interface; the GPU is idle and no buffer references remain. The flags
        // must match creation.
        let hr = unsafe {
            (s.vtbl().ResizeBuffers)(
                s.as_raw(),
                swapchain.image_count,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                swapchain.flags,
            )
        };
        if let Err(e) = swapchain.lost.result(hr) {
            // A failed ResizeBuffers leaves the old buffers: wrap them again so the swapchain
            // keeps presenting at its old size (or stays image-less until a resize succeeds).
            let _ = swapchain.create_views();
            return Err(e);
        }
        swapchain.width = width;
        swapchain.height = height;
        swapchain.create_views()
    }

    fn set_present_mode(swapchain: &mut Swapchain, mode: PresentMode) {
        swapchain.present_mode = mode;
    }

    fn set_max_frame_latency(swapchain: &mut Swapchain, frames: u32) {
        let s = &swapchain.swapchain;
        // SAFETY: valid interface, created with the waitable flag. It only fails for a
        // latency out of range, which the caller clamped.
        let _ = unsafe { (s.vtbl().SetMaximumFrameLatency)(s.as_raw(), frames) };
    }

    fn supports_color_space(swapchain: &Swapchain, color_space: ColorSpace) -> bool {
        let mut support = 0;
        let s = &swapchain.swapchain;
        // SAFETY: valid interface and out pointer.
        let hr = unsafe {
            (s.vtbl().CheckColorSpaceSupport)(
                s.as_raw(),
                dxgi_color_space(color_space),
                &mut support,
            )
        };
        hr >= 0 && support & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT != 0
    }

    fn set_color_space(swapchain: &mut Swapchain, color_space: ColorSpace) -> Result<()> {
        let s = &swapchain.swapchain;
        // SAFETY: valid interface; support was checked by `api.rs`.
        check(unsafe { (s.vtbl().SetColorSpace1)(s.as_raw(), dxgi_color_space(color_space)) })?;
        swapchain.color_space = color_space;
        Ok(())
    }

    fn set_hdr_metadata(swapchain: &mut Swapchain, metadata: Option<&HdrMetadata>) -> Result<()> {
        let s4 = swapchain
            .swapchain4
            .as_ref()
            .ok_or(Error::from_kind(ErrorKind::Unsupported))?;
        let hr = match metadata {
            None => {
                // SAFETY: valid interface; no metadata.
                unsafe {
                    (s4.vtbl().SetHDRMetaData)(
                        s4.as_raw(),
                        DXGI_HDR_METADATA_TYPE_NONE,
                        0,
                        core::ptr::null(),
                    )
                }
            }
            Some(m) => {
                let hdr10 = DXGI_HDR_METADATA_HDR10 {
                    RedPrimary: chromaticity(m.red_primary),
                    GreenPrimary: chromaticity(m.green_primary),
                    BluePrimary: chromaticity(m.blue_primary),
                    WhitePoint: chromaticity(m.white_point),
                    // Whole nits, and 0.0001-nit units for the minimum.
                    MaxMasteringLuminance: nearest(m.max_mastering_luminance, 1e9),
                    MinMasteringLuminance: nearest(m.min_mastering_luminance * 10000.0, 1e9),
                    MaxContentLightLevel: nearest(m.max_content_light_level, 65535.0) as u16,
                    MaxFrameAverageLightLevel: nearest(m.max_frame_average_light_level, 65535.0)
                        as u16,
                };
                // SAFETY: valid interface; the metadata struct and its size.
                unsafe {
                    (s4.vtbl().SetHDRMetaData)(
                        s4.as_raw(),
                        DXGI_HDR_METADATA_TYPE_HDR10,
                        size_of::<DXGI_HDR_METADATA_HDR10>() as u32,
                        (&raw const hdr10).cast(),
                    )
                }
            }
        };
        check(hr)
    }

    fn containing_output(swapchain: &Swapchain) -> Result<OutputInfo> {
        let mut output = core::ptr::null_mut();
        let s = &swapchain.swapchain;
        // SAFETY: valid interface and out pointer.
        check(unsafe { (s.vtbl().GetContainingOutput)(s.as_raw(), &mut output) })?;
        output_info(output).ok_or(Error::from_kind(ErrorKind::Unsupported))
    }

    fn frame_statistics(swapchain: &Swapchain) -> Result<Option<FrameStatistics>> {
        let mut stats = DXGI_FRAME_STATISTICS::default();
        let s = &swapchain.swapchain;
        // SAFETY: valid interface and out pointer.
        match unsafe { (s.vtbl().GetFrameStatistics)(s.as_raw(), &mut stats) } {
            // Nothing presented yet, or the counters were reset.
            DXGI_ERROR_FRAME_STATISTICS_DISJOINT => Ok(None),
            hr => {
                check(hr)?;
                Ok(Some(FrameStatistics {
                    present_count: stats.PresentCount,
                    present_refresh_count: stats.PresentRefreshCount,
                    sync_refresh_count: stats.SyncRefreshCount,
                    sync_cpu_time: stats.SyncQPCTime as u64,
                }))
            }
        }
    }

    fn size(swapchain: &Swapchain) -> (u32, u32) {
        (swapchain.width, swapchain.height)
    }

    fn format(swapchain: &Swapchain) -> Format {
        swapchain.format
    }

    fn color_space(swapchain: &Swapchain) -> ColorSpace {
        swapchain.color_space
    }

    fn textures(swapchain: &Swapchain) -> &[Texture] {
        &swapchain.textures
    }

    fn views(swapchain: &Swapchain) -> &[RenderTargetView] {
        &swapchain.views
    }
}
