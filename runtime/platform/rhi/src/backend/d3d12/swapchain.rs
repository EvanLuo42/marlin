//! Flip-model DXGI swapchain with a frame-latency waitable object.

use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::com::{self, *};
use super::command::{RenderTargetView as RawView, Texture as RawTexture};
use super::ffi::*;
use super::{Backend, Device, Handle, Idle, Queue, Validation, check, error, flush, wait_handle};
use crate::{
    Error, ErrorKind, Format, PresentMode, PresentStatus, RenderTargetView, Result, SurfaceHandle,
    SwapchainDesc, Texture,
};

pub(crate) struct Swapchain {
    swapchain: Com<IDXGISwapChain3>,
    device: Com<ID3D12Device>,
    rtv_heap: Com<ID3D12DescriptorHeap>,
    rtv_size: u32,
    waitable: Handle,
    flags: u32,
    format: Format,
    width: u32,
    height: u32,
    present_mode: PresentMode,
    tearing: bool,
    textures: Vec<Texture>,
    views: Vec<RenderTargetView>,
    idle: Idle,
    validation: Option<Arc<Validation>>,
}

fn dxgi_format(format: Format) -> u32 {
    match format {
        Format::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        Format::Rgba8UnormSrgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        Format::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        Format::Bgra8UnormSrgb => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        Format::Rgb10a2Unorm => DXGI_FORMAT_R10G10B10A2_UNORM,
        Format::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
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
        if desc.width == 0 || desc.height == 0 || !(2..=16).contains(&desc.image_count) {
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
        check(unsafe {
            (factory.CreateSwapChainForHwnd)(
                device.factory.as_raw(),
                queue.queue.as_raw(),
                hwnd.as_ptr(),
                &dxgi_desc,
                core::ptr::null(),
                core::ptr::null_mut(),
                &mut swapchain1,
            )
        })?;
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
            swapchain,
            device: device.device.clone(),
            rtv_heap,
            rtv_size: device.rtv_size,
            waitable,
            flags,
            format: desc.format,
            width: desc.width,
            height: desc.height,
            present_mode: desc.present_mode,
            tearing,
            textures: Vec::with_capacity(desc.image_count as usize),
            views: Vec::with_capacity(desc.image_count as usize),
            idle: Idle::new(device, queue.queue.clone())?,
            validation: device.validation.clone(),
        };
        swapchain.create_views(desc.image_count)?;
        Ok(swapchain)
    }

    fn create_views(&mut self, count: u32) -> Result<()> {
        let heap = &self.rtv_heap;
        let mut start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        // SAFETY: valid interface; the struct is returned through `start`.
        unsafe { (heap.vtbl().GetCPUDescriptorHandleForHeapStart)(heap.as_raw(), &mut start) };
        let view_desc = D3D12_RENDER_TARGET_VIEW_DESC {
            Format: dxgi_format(self.format),
            ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
            Texture2D: [0, 0],
            _union: [0, 0],
            _align: [],
        };
        for i in 0..count {
            let s = &self.swapchain;
            // SAFETY: valid interface, IID and out pointer; `i` < the buffer count.
            let resource = com::create::<ID3D12Resource>(|iid, out| unsafe {
                (s.vtbl().GetBuffer)(s.as_raw(), i, iid, out)
            })
            .map_err(error)?;
            let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: start.ptr + (i * self.rtv_size) as usize,
            };
            let d = &self.device;
            // SAFETY: valid device and resource; `handle` is inside the heap's `count` slots.
            unsafe {
                (d.vtbl().CreateRenderTargetView)(d.as_raw(), resource.as_raw(), &view_desc, handle)
            };
            self.textures.push(Texture {
                raw: RawTexture {
                    resource,
                    mip_levels: 1,
                    array_layers: 1,
                },
            });
            self.views.push(RenderTargetView {
                raw: RawView { handle },
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
            DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET => {
                let d = &swapchain.device;
                // SAFETY: valid interface.
                let reason = unsafe { (d.vtbl().GetDeviceRemovedReason)(d.as_raw()) };
                marlin_diag::error!("D3D12 device removed: {:#010x}", reason as u32);
                Err(error(hr))
            }
            hr => check(hr).map(|()| PresentStatus::Presented),
        }
    }

    fn resize(swapchain: &mut Swapchain, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        if (width, height) == (swapchain.width, swapchain.height) {
            return Ok(());
        }
        swapchain.idle.wait()?;
        // ResizeBuffers fails while any reference to the old buffers remains.
        let count = swapchain.textures.len() as u32;
        swapchain.textures.clear();
        swapchain.views.clear();
        let s = &swapchain.swapchain;
        // SAFETY: valid interface; the GPU is idle and no buffer references remain. The flags
        // must match creation.
        check(unsafe {
            (s.vtbl().ResizeBuffers)(
                s.as_raw(),
                count,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                swapchain.flags,
            )
        })?;
        swapchain.width = width;
        swapchain.height = height;
        swapchain.create_views(count)
    }

    fn set_present_mode(swapchain: &mut Swapchain, mode: PresentMode) {
        swapchain.present_mode = mode;
    }

    fn size(swapchain: &Swapchain) -> (u32, u32) {
        (swapchain.width, swapchain.height)
    }

    fn format(swapchain: &Swapchain) -> Format {
        swapchain.format
    }

    fn textures(swapchain: &Swapchain) -> &[Texture] {
        &swapchain.textures
    }

    fn views(swapchain: &Swapchain) -> &[RenderTargetView] {
        &swapchain.views
    }
}
