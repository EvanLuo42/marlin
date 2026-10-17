//! D3D12 backend (Windows; the Xbox backend builds on it).
//!
//! DXGI and D3D12 are loaded from System32 at run time, so a machine without them gets
//! `Unsupported` instead of a loader error. Baseline: feature level 12_0. Enhanced barriers are
//! used when the runtime and driver report them (`D3D12_OPTIONS12`); otherwise barriers become
//! legacy resource-state transitions.

mod com;
mod command;
mod diag;
mod features;
mod ffi;
mod pipeline;
mod raytracing;
mod resource;
mod swapchain;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec::Vec;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::core::{BOOL, HRESULT};

use com::*;
use ffi::*;

use core::ptr::NonNull;

use crate::contract::{Fences, Object, Queues, Rhi, Types};
use crate::{
    AdapterInfo, AdapterKind, AdapterPreference, BufferDesc, ClockCalibration, ColorSpace,
    DeviceDesc, Error, ErrorKind, Fence as PublicFence, FenceWait, InstanceDesc, MemoryBudget,
    MemoryUsage, OutputInfo, Pageable, QueryKind, QueryPoolDesc, QueueType, Rect,
    ResidencyPriority, Result, SparseBind, SparseTextureRegion, SubmitDesc, TextureDesc,
};
use diag::{Lost, Validation, flush};
use pipeline::Bindless;
use resource::Descriptor;

pub(crate) struct Backend;

impl Types for Backend {
    const SHADER_FORMAT: crate::ShaderFormat = crate::ShaderFormat::Dxil;
    type Instance = Instance;
    type Device = Device;
    type Queue = Queue;
    type Fence = Fence;
    type CommandPool = command::CommandPool;
    type CommandList = command::CommandList;
    type Swapchain = swapchain::Swapchain;
    type Buffer = resource::Buffer;
    type Texture = resource::Texture;
    type RenderTargetView = resource::RenderTargetView;
    type DepthStencilView = resource::DepthStencilView;
    type ShaderView = Descriptor;
    type Sampler = Descriptor;
    type GraphicsPipeline = pipeline::GraphicsPipeline;
    type ComputePipeline = pipeline::ComputePipeline;
    type MeshPipeline = pipeline::MeshPipeline;
    type QueryPool = QueryPool;
    type Heap = resource::Heap;
    type PipelineCache = pipeline::PipelineCache;
    type AccelerationStructure = raytracing::AccelerationStructure;
    type SharedHandle = Handle;
}

impl Rhi for Backend {}

/// Maps an `HRESULT` failure to an [`Error`].
fn error(hr: HRESULT) -> Error {
    let kind = match hr {
        E_OUTOFMEMORY => ErrorKind::OutOfMemory,
        E_INVALIDARG | DXGI_ERROR_INVALID_CALL => ErrorKind::InvalidInput,
        DXGI_ERROR_DEVICE_REMOVED
        | DXGI_ERROR_DEVICE_HUNG
        | DXGI_ERROR_DEVICE_RESET
        | DXGI_ERROR_DRIVER_INTERNAL_ERROR => ErrorKind::DeviceLost,
        DXGI_ERROR_UNSUPPORTED | E_NOINTERFACE | E_NOTIMPL => ErrorKind::Unsupported,
        DXGI_ERROR_NOT_FOUND => ErrorKind::NotFound,
        _ => ErrorKind::Other,
    };
    Error::new(kind, hr)
}

fn check(hr: HRESULT) -> Result<()> {
    if hr < 0 { Err(error(hr)) } else { Ok(()) }
}

fn unsupported() -> Error {
    Error::from_kind(ErrorKind::Unsupported)
}

fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

/// A kernel handle, closed on drop. Also the backend's shared handle.
pub(crate) struct Handle(HANDLE);

// SAFETY: kernel handles are process-wide.
unsafe impl Send for Handle {}
// SAFETY: as above; waiting on a handle from several threads is allowed.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A new unnamed auto-reset event.
fn event() -> Result<Handle> {
    // SAFETY: unnamed auto-reset event with default security.
    let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
    if event.is_null() {
        return Err(Error::from_kind(ErrorKind::OutOfMemory));
    }
    Ok(Handle(event))
}

/// Waits on `handle` for up to `timeout`. `Ok(false)` on timeout.
fn wait_handle(handle: HANDLE, timeout: Option<Duration>) -> Result<bool> {
    let ms = marlin_pal::os::windows::timeout_ms(timeout);
    // SAFETY: `handle` is a live waitable handle.
    match unsafe { WaitForSingleObject(handle, ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(Error::from_kind(ErrorKind::Other)),
    }
}

/// `s` as a `String`-free `&str` in `buf`: UTF-16 up to the first NUL.
fn utf16_to_str<'a>(s: &[u16], buf: &'a mut [u8]) -> &'a str {
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

// --- Instance ---

pub(crate) struct Instance {
    factory: Com<IDXGIFactory4>,
    factory6: Option<Com<IDXGIFactory6>>,
    tearing: bool,
    validation: bool,
}

impl Instance {
    fn enum_adapter(&self, index: u32) -> Option<Com<IDXGIAdapter1>> {
        match &self.factory6 {
            Some(f) => com::create(|iid, out| {
                // SAFETY: valid interface, IID and out pointer.
                unsafe {
                    (f.vtbl().EnumAdapterByGpuPreference)(
                        f.as_raw(),
                        index,
                        DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                        iid,
                        out,
                    )
                }
            })
            .ok(),
            None => {
                let f = &self.factory;
                let mut out = ptr::null_mut();
                // SAFETY: valid interface and out pointer; EnumAdapters1 returns IDXGIAdapter1.
                let hr = unsafe { (f.vtbl().base.EnumAdapters1)(f.as_raw(), index, &mut out) };
                // SAFETY: on success `out` is an owned IDXGIAdapter1.
                (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten()
            }
        }
    }

    fn warp_adapter(&self) -> Result<Com<IDXGIAdapter1>> {
        let f = &self.factory;
        // SAFETY: valid interface, IID and out pointer.
        com::create(|iid, out| unsafe { (f.vtbl().EnumWarpAdapter)(f.as_raw(), iid, out) })
            .map_err(error)
    }
}

fn adapter_desc(adapter: &Com<IDXGIAdapter1>) -> Option<DXGI_ADAPTER_DESC1> {
    // SAFETY: all-zero is a valid DXGI_ADAPTER_DESC1.
    let mut desc: DXGI_ADAPTER_DESC1 = unsafe { core::mem::zeroed() };
    // SAFETY: valid interface and out pointer.
    (unsafe { (adapter.vtbl().GetDesc1)(adapter.as_raw(), &mut desc) } >= 0).then_some(desc)
}

/// The adapter's PCI ids and its user-mode driver's version: what makes compiled pipelines
/// portable between runs.
fn cache_identity(adapter: &Com<IDXGIAdapter1>) -> [u64; 3] {
    let desc = adapter_desc(adapter);
    let mut driver = 0i64;
    // SAFETY: valid interface, IID and out pointer.
    unsafe {
        (adapter.vtbl().CheckInterfaceSupport)(adapter.as_raw(), &IID_IDXGI_DEVICE, &mut driver)
    };
    desc.map_or([0; 3], |d| {
        [
            (d.VendorId as u64) << 32 | d.DeviceId as u64,
            (d.SubSysId as u64) << 32 | d.Revision as u64,
            driver as u64,
        ]
    })
}

fn luid(desc: &DXGI_ADAPTER_DESC1) -> u64 {
    (desc.AdapterLuid.HighPart as u32 as u64) << 32 | desc.AdapterLuid.LowPart as u64
}

fn adapter_info(adapter: &Com<IDXGIAdapter1>, index: u32) -> Option<AdapterInfo> {
    let desc = adapter_desc(adapter)?;
    let kind = if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0 {
        AdapterKind::Software
    } else {
        AdapterKind::Hardware
    };
    let mut name = [0u8; 256];
    let mut info = AdapterInfo::new(index, kind, utf16_to_str(&desc.Description, &mut name));
    info.vendor_id = desc.VendorId;
    info.device_id = desc.DeviceId;
    info.dedicated_video_memory = desc.DedicatedVideoMemory as u64;
    info.shared_system_memory = desc.SharedSystemMemory as u64;
    info.luid = luid(&desc);
    Some(info)
}

fn color_space(dxgi: i32) -> ColorSpace {
    match dxgi {
        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 => ColorSpace::Hdr10,
        DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 => ColorSpace::ScRgb,
        _ => ColorSpace::Srgb,
    }
}

/// What an output (from `EnumOutputs` or `GetContainingOutput`) reports.
fn output_info(output: *mut c_void) -> Option<OutputInfo> {
    // SAFETY: `output` is an owned IDXGIOutput.
    let output = unsafe { Com::<IDXGIOutput>::from_raw(output) }?;
    let output6 = output.cast::<IDXGIOutput6>().ok()?;
    // SAFETY: all-zero is a valid DXGI_OUTPUT_DESC1.
    let mut d: DXGI_OUTPUT_DESC1 = unsafe { core::mem::zeroed() };
    // SAFETY: valid interface and out pointer.
    if unsafe { (output6.vtbl().GetDesc1)(output6.as_raw(), &mut d) } < 0 {
        return None;
    }
    let r = d.DesktopCoordinates;
    let desktop = Rect {
        x: r.left,
        y: r.top,
        width: (r.right - r.left).max(0) as u32,
        height: (r.bottom - r.top).max(0) as u32,
    };
    let mut name = [0u8; 64];
    let mut info = OutputInfo::new(utf16_to_str(&d.DeviceName, &mut name), desktop);
    info.bits_per_color = d.BitsPerColor;
    info.color_space = color_space(d.ColorSpace);
    info.red_primary = d.RedPrimary;
    info.green_primary = d.GreenPrimary;
    info.blue_primary = d.BluePrimary;
    info.white_point = d.WhitePoint;
    info.min_luminance = d.MinLuminance;
    info.max_luminance = d.MaxLuminance;
    info.max_full_frame_luminance = d.MaxFullFrameLuminance;
    Some(info)
}

impl crate::contract::Instance for Backend {
    fn create_instance(desc: &InstanceDesc) -> Result<Instance> {
        if desc.validation {
            diag::enable_debug_layer(desc.gpu_validation);
        }
        if desc.crash_diagnostics {
            diag::enable_crash_diagnostics();
        }
        let create = create_dxgi_factory2().ok_or_else(unsupported)?;
        let make = |flags| {
            // SAFETY: valid IID and out pointer.
            com::create::<IDXGIFactory4>(|iid, out| unsafe { create(flags, iid, out) })
        };
        // The debug factory needs the Graphics Tools optional feature; fall back without it.
        let factory = match desc.validation {
            true => make(DXGI_CREATE_FACTORY_DEBUG).or_else(|_| make(0)),
            false => make(0),
        }
        .map_err(error)?;

        let tearing = factory.cast::<IDXGIFactory5>().is_ok_and(|f| {
            let mut allow: BOOL = 0;
            // SAFETY: valid interface; `allow` is the BOOL this feature reports.
            let hr = unsafe {
                (f.vtbl().CheckFeatureSupport)(
                    f.as_raw(),
                    DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                    (&raw mut allow).cast(),
                    size_of::<BOOL>() as u32,
                )
            };
            hr >= 0 && allow != 0
        });
        Ok(Instance {
            factory6: factory.cast().ok(),
            factory,
            tearing,
            validation: desc.validation,
        })
    }

    fn adapter(instance: &Instance, index: u32) -> Option<AdapterInfo> {
        adapter_info(&instance.enum_adapter(index)?, index)
    }

    fn output(instance: &Instance, adapter: u32, output: u32) -> Option<OutputInfo> {
        let a = instance.enum_adapter(adapter)?;
        let mut out = ptr::null_mut();
        // SAFETY: valid interface and out pointer.
        if unsafe { (a.vtbl().EnumOutputs)(a.as_raw(), output, &mut out) } < 0 {
            return None;
        }
        output_info(out)
    }

    fn create_device(instance: &Instance, desc: &DeviceDesc) -> Result<(Device, Queues<Queue>)> {
        let not_found = || Error::from_kind(ErrorKind::NotFound);
        let (adapter, device, index) = match desc.adapter {
            AdapterPreference::HighPerformance => {
                let mut found = None;
                for index in 0.. {
                    let Some(adapter) = instance.enum_adapter(index) else {
                        break;
                    };
                    let hardware = adapter_info(&adapter, index)
                        .is_some_and(|info| info.kind == AdapterKind::Hardware);
                    if let (true, Ok(device)) = (hardware, features::create_d3d12_device(&adapter))
                    {
                        found = Some((adapter, device, index));
                        break;
                    }
                }
                found.ok_or_else(not_found)?
            }
            AdapterPreference::Software => {
                let adapter = instance.warp_adapter()?;
                let device = features::create_d3d12_device(&adapter)?;
                // Not in the enumeration order; report it past the end.
                let index = (0..)
                    .find(|&i| instance.enum_adapter(i).is_none())
                    .unwrap_or(0);
                (adapter, device, index)
            }
            AdapterPreference::Index(index) => {
                let adapter = instance.enum_adapter(index).ok_or_else(not_found)?;
                let device = features::create_d3d12_device(&adapter)?;
                (adapter, device, index)
            }
            AdapterPreference::Luid(id) => {
                let index = (0..)
                    .map_while(|i| instance.enum_adapter(i).map(|a| (i, a)))
                    .find(|(_, a)| adapter_desc(a).is_some_and(|d| luid(&d) == id))
                    .map(|(i, _)| i)
                    .ok_or_else(not_found)?;
                let adapter = instance.enum_adapter(index).ok_or_else(not_found)?;
                let device = features::create_d3d12_device(&adapter)?;
                (adapter, device, index)
            }
        };
        let (device, model) = device;
        let info = adapter_info(&adapter, index).ok_or(Error::from_kind(ErrorKind::Other))?;

        let validation = instance
            .validation
            .then(|| device.cast::<ID3D12InfoQueue>().ok())
            .flatten()
            .map(|info_queue| {
                Arc::new(Validation {
                    info_queue,
                    lock: AtomicBool::new(false),
                    errors: AtomicU64::new(0),
                })
            });

        let features = features::query_features(&device, model, instance.tearing, desc);
        let cache_identity = cache_identity(&adapter);
        let device5 = device.cast::<ID3D12Device5>().map_err(error)?;
        let device10 = match features.enhanced_barriers {
            true => device.cast().ok(),
            false => None,
        };

        let device = Device {
            // SAFETY: valid interface.
            rtv_size: unsafe {
                (device.vtbl().GetDescriptorHandleIncrementSize)(
                    device.as_raw(),
                    D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                )
            },
            bindless: Bindless::new(&device)?,
            lost: Arc::new(Lost::new(device.clone())),
            device,
            device10,
            device5,
            adapter: adapter.cast().ok(),
            factory: instance.factory.clone(),
            info,
            features,
            validation,
            cache_identity,
        };
        let queues = Queues {
            graphics: Queue::new(&device, QueueType::Graphics)?,
            compute: Some(Queue::new(&device, QueueType::Compute)?),
            copy: Some(Queue::new(&device, QueueType::Copy)?),
        };
        Ok((device, queues))
    }
}

// --- Device ---

pub(crate) struct Device {
    device: Com<ID3D12Device>,
    /// Pipeline state streams and libraries, residency priorities, ray tracing, `RemoveDevice`.
    device5: Com<ID3D12Device5>,
    /// Resources created with barrier layouts, where enhanced barriers are in use.
    device10: Option<Com<ID3D12Device10>>,
    /// The memory budget (`IDXGIAdapter3`, Windows 10).
    adapter: Option<Com<IDXGIAdapter3>>,
    factory: Com<IDXGIFactory4>,
    info: AdapterInfo,
    features: crate::Features,
    rtv_size: u32,
    bindless: Arc<Bindless>,
    validation: Option<Arc<Validation>>,
    lost: Arc<Lost>,
    /// What pipeline caches are valid for: the adapter's PCI ids and the driver version.
    pub(super) cache_identity: [u64; 3],
}

impl Drop for Device {
    fn drop(&mut self) {
        flush(&self.validation);
    }
}

fn create_fence(device: &Device, initial: u64, shared: bool) -> Result<Fence> {
    let d = &device.device;
    let flags = if shared { D3D12_FENCE_FLAG_SHARED } else { 0 };
    // SAFETY: valid interface, IID and out pointer.
    let fence = com::create::<ID3D12Fence>(|iid, out| unsafe {
        (d.vtbl().CreateFence)(d.as_raw(), initial, flags, iid, out)
    })
    .map_err(|hr| device.lost.result(hr).unwrap_err())?;
    Ok(Fence {
        fence,
        lost: device.lost.clone(),
    })
}

/// The objects whose residency `objects` names.
fn pageables(objects: &[Pageable<'_>]) -> Vec<*mut c_void> {
    objects
        .iter()
        .map(|o| match o {
            Pageable::Heap(h) => h.raw.heap.as_raw(),
            Pageable::Buffer(b) => b.raw.resource.as_raw(),
            Pageable::Texture(t) => t.raw.resource.as_raw(),
            Pageable::AccelerationStructure(a) => a.buffer().raw.resource.as_raw(),
        })
        .collect()
}

impl crate::contract::Device for Backend {
    fn adapter_info(device: &Device) -> &AdapterInfo {
        &device.info
    }

    fn features(device: &Device) -> crate::Features {
        device.features
    }

    fn format_support(device: &Device, format: crate::Format) -> crate::FormatSupport {
        features::format_support(device, format)
    }

    fn status(device: &Device) -> Result<()> {
        device.lost.check()
    }

    fn force_device_lost(device: &Device) {
        let d = &device.device5;
        // SAFETY: valid interface.
        unsafe { (d.vtbl().RemoveDevice)(d.as_raw()) };
    }

    fn set_stable_power_state(device: &Device, stable: bool) -> Result<()> {
        let d = &device.device;
        // SAFETY: valid interface.
        check(unsafe { (d.vtbl().SetStablePowerState)(d.as_raw(), stable as BOOL) })
    }

    fn create_fence(device: &Device, initial: u64, shared: bool) -> Result<Fence> {
        create_fence(device, initial, shared)
    }

    fn wait_for_fences(
        device: &Device,
        fences: &[(&PublicFence, u64)],
        mode: FenceWait,
        timeout: Option<Duration>,
    ) -> Result<bool> {
        let done = |(f, v): &(&PublicFence, u64)| Backend::completed_value(&f.raw) >= *v;
        let finished = match mode {
            FenceWait::All => fences.iter().all(done),
            FenceWait::Any => fences.iter().any(done),
        };
        if !finished {
            let raw: Vec<*mut c_void> = fences.iter().map(|(f, _)| f.raw.fence.as_raw()).collect();
            let values: Vec<u64> = fences.iter().map(|(_, v)| *v).collect();
            let event = event()?;
            let flags = match mode {
                FenceWait::All => 0,
                FenceWait::Any => D3D12_MULTIPLE_FENCE_WAIT_FLAG_ANY,
            };
            let d = &device.device5;
            // SAFETY: valid interfaces and event; the arrays hold `fences.len()` entries.
            check(unsafe {
                (d.vtbl().SetEventOnMultipleFenceCompletion)(
                    d.as_raw(),
                    raw.as_ptr(),
                    values.as_ptr(),
                    raw.len() as u32,
                    flags,
                    event.0,
                )
            })?;
            if !wait_handle(event.0, timeout)? {
                return Ok(false);
            }
        }
        // A removed device completes every fence at u64::MAX.
        if fences
            .iter()
            .any(|(f, _)| Backend::completed_value(&f.raw) == u64::MAX)
        {
            device.lost.check()?;
        }
        Ok(true)
    }

    fn create_command_pool(device: &Device, ty: QueueType) -> Result<command::CommandPool> {
        command::CommandPool::new(&device.device, ty)
    }

    fn create_command_list(
        device: &Device,
        pool: &command::CommandPool,
    ) -> Result<command::CommandList> {
        command::CommandList::new(device, pool)
    }

    unsafe fn create_swapchain(
        device: &Device,
        queue: &Queue,
        surface: crate::SurfaceHandle,
        desc: &crate::SwapchainDesc,
    ) -> Result<swapchain::Swapchain> {
        // SAFETY: forwarded from the caller.
        unsafe { swapchain::Swapchain::new(device, queue, surface, desc) }
    }

    fn validation_errors(device: &Device) -> u64 {
        device.validation.as_ref().map_or(0, |v| {
            v.flush();
            v.errors.load(Ordering::Relaxed)
        })
    }

    fn create_buffer(device: &Device, desc: &BufferDesc) -> Result<resource::Buffer> {
        resource::create_buffer(device, desc)
    }

    fn create_texture(device: &Device, desc: &TextureDesc) -> Result<resource::Texture> {
        resource::create_texture(device, desc)
    }

    fn create_render_target_view(
        device: &Device,
        texture: &crate::Texture,
        desc: &crate::RenderTargetViewDesc,
    ) -> Result<resource::RenderTargetView> {
        resource::create_render_target_view(device, texture, desc)
    }

    fn create_depth_stencil_view(
        device: &Device,
        texture: &crate::Texture,
        desc: &crate::DepthStencilViewDesc,
    ) -> Result<resource::DepthStencilView> {
        resource::create_depth_stencil_view(device, texture, desc)
    }

    fn create_texture_view(
        device: &Device,
        texture: &crate::Texture,
        desc: &crate::TextureViewDesc,
    ) -> Result<Descriptor> {
        resource::create_texture_view(device, texture, desc)
    }

    fn create_buffer_view(
        device: &Device,
        buffer: &crate::Buffer,
        desc: &crate::BufferViewDesc,
    ) -> Result<Descriptor> {
        resource::create_buffer_view(device, buffer, desc)
    }

    fn create_sampler(device: &Device, desc: &crate::SamplerDesc) -> Result<Descriptor> {
        resource::create_sampler(device, desc)
    }

    fn create_graphics_pipeline(
        device: &Device,
        desc: &crate::GraphicsPipelineDesc<'_>,
    ) -> Result<pipeline::GraphicsPipeline> {
        let pipeline = pipeline::create_graphics_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_compute_pipeline(
        device: &Device,
        desc: &crate::ComputePipelineDesc<'_>,
    ) -> Result<pipeline::ComputePipeline> {
        let pipeline = pipeline::create_compute_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_mesh_pipeline(
        device: &Device,
        desc: &crate::MeshPipelineDesc<'_>,
    ) -> Result<pipeline::MeshPipeline> {
        let pipeline = pipeline::create_mesh_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_pipeline_cache(device: &Device, data: &[u8]) -> Result<pipeline::PipelineCache> {
        pipeline::PipelineCache::new(device, data)
    }

    fn serialize_pipeline_cache(cache: &pipeline::PipelineCache) -> Result<Vec<u8>> {
        cache.serialize()
    }

    fn create_query_pool(device: &Device, desc: &QueryPoolDesc) -> Result<QueryPool> {
        let (heap_type, query_type) = match desc.kind {
            QueryKind::Timestamp => (D3D12_QUERY_HEAP_TYPE_TIMESTAMP, D3D12_QUERY_TYPE_TIMESTAMP),
            QueryKind::CopyTimestamp => (
                D3D12_QUERY_HEAP_TYPE_COPY_QUEUE_TIMESTAMP,
                D3D12_QUERY_TYPE_TIMESTAMP,
            ),
            QueryKind::Occlusion => (D3D12_QUERY_HEAP_TYPE_OCCLUSION, D3D12_QUERY_TYPE_OCCLUSION),
            QueryKind::BinaryOcclusion => (
                D3D12_QUERY_HEAP_TYPE_OCCLUSION,
                D3D12_QUERY_TYPE_BINARY_OCCLUSION,
            ),
            QueryKind::PipelineStatistics => (
                D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1,
                D3D12_QUERY_TYPE_PIPELINE_STATISTICS1,
            ),
        };
        let heap_desc = D3D12_QUERY_HEAP_DESC {
            Type: heap_type,
            Count: desc.count,
            NodeMask: 0,
        };
        let d = &device.device;
        // SAFETY: valid interface, description, IID and out pointer.
        let heap = com::create(|iid, out| unsafe {
            (d.vtbl().CreateQueryHeap)(d.as_raw(), &heap_desc, iid, out)
        })
        .map_err(error)?;
        Ok(QueryPool { heap, query_type })
    }

    fn create_heap(device: &Device, desc: &crate::HeapDesc) -> Result<resource::Heap> {
        resource::create_heap(device, desc)
    }

    fn buffer_requirements(device: &Device, desc: &BufferDesc) -> crate::MemoryRequirements {
        resource::buffer_requirements(device, desc)
    }

    fn texture_requirements(device: &Device, desc: &TextureDesc) -> crate::MemoryRequirements {
        resource::texture_requirements(device, desc)
    }

    fn create_placed_buffer(
        device: &Device,
        heap: &resource::Heap,
        offset: u64,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        resource::create_placed_buffer(device, heap, offset, desc)
    }

    fn create_placed_texture(
        device: &Device,
        heap: &resource::Heap,
        offset: u64,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        resource::create_placed_texture(device, heap, offset, desc)
    }

    fn memory_budget(device: &Device) -> Result<MemoryBudget> {
        let adapter = device.adapter.as_ref().ok_or_else(unsupported)?;
        let query = |group| {
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            // SAFETY: valid interface and out pointer; node 0.
            check(unsafe {
                (adapter.vtbl().QueryVideoMemoryInfo)(adapter.as_raw(), 0, group, &mut info)
            })?;
            Ok(MemoryUsage {
                budget: info.Budget,
                usage: info.CurrentUsage,
                available_for_reservation: info.AvailableForReservation,
                reservation: info.CurrentReservation,
            })
        };
        Ok(MemoryBudget {
            local: query(DXGI_MEMORY_SEGMENT_GROUP_LOCAL)?,
            non_local: query(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL)?,
        })
    }

    fn set_memory_reservation(device: &Device, local: u64, non_local: u64) -> Result<()> {
        let adapter = device.adapter.as_ref().ok_or_else(unsupported)?;
        for (group, bytes) in [
            (DXGI_MEMORY_SEGMENT_GROUP_LOCAL, local),
            (DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, non_local),
        ] {
            // SAFETY: valid interface; node 0.
            check(unsafe {
                (adapter.vtbl().SetVideoMemoryReservation)(adapter.as_raw(), 0, group, bytes)
            })?;
        }
        Ok(())
    }

    fn make_resident(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
        let raw = pageables(objects);
        let d = &device.device;
        // SAFETY: valid interface; `raw` holds live pageable objects.
        device
            .lost
            .result(unsafe { (d.vtbl().MakeResident)(d.as_raw(), raw.len() as u32, raw.as_ptr()) })
    }

    fn evict(device: &Device, objects: &[Pageable<'_>]) -> Result<()> {
        let raw = pageables(objects);
        let d = &device.device;
        // SAFETY: valid interface; `raw` holds live pageable objects.
        device
            .lost
            .result(unsafe { (d.vtbl().Evict)(d.as_raw(), raw.len() as u32, raw.as_ptr()) })
    }

    fn set_residency_priority(
        device: &Device,
        objects: &[Pageable<'_>],
        priority: ResidencyPriority,
    ) -> Result<()> {
        let raw = pageables(objects);
        let value = match priority {
            ResidencyPriority::Minimum => D3D12_RESIDENCY_PRIORITY_MINIMUM,
            ResidencyPriority::Low => D3D12_RESIDENCY_PRIORITY_LOW,
            ResidencyPriority::Normal => D3D12_RESIDENCY_PRIORITY_NORMAL,
            ResidencyPriority::High => D3D12_RESIDENCY_PRIORITY_HIGH,
            ResidencyPriority::Maximum => D3D12_RESIDENCY_PRIORITY_MAXIMUM,
        };
        let priorities = alloc::vec![value; raw.len()];
        let d = &device.device5;
        // SAFETY: valid interface; both arrays hold `raw.len()` entries.
        check(unsafe {
            (d.vtbl().SetResidencyPriority)(
                d.as_raw(),
                raw.len() as u32,
                raw.as_ptr(),
                priorities.as_ptr(),
            )
        })
    }

    fn acceleration_structure_sizes(
        device: &Device,
        inputs: &crate::AccelerationStructureInputs<'_>,
        flags: crate::BuildFlags,
    ) -> crate::AccelerationStructureSizes {
        raytracing::sizes(device, inputs, flags)
    }

    fn create_acceleration_structure(
        device: &Device,
        storage: &resource::Buffer,
        offset: u64,
        _kind: crate::AccelerationStructureKind,
    ) -> Result<(resource::Buffer, raytracing::AccelerationStructure)> {
        Ok((
            storage.clone(),
            raytracing::create(device, storage, offset)?,
        ))
    }

    fn create_sparse_buffer(device: &Device, desc: &BufferDesc) -> Result<resource::Buffer> {
        resource::create_sparse_buffer(device, desc)
    }

    fn create_sparse_texture(device: &Device, desc: &TextureDesc) -> Result<resource::Texture> {
        resource::create_sparse_texture(device, desc)
    }

    fn sparse_texture_layout(_: &Device, texture: &crate::Texture) -> crate::SparseTextureLayout {
        texture.raw.sparse.unwrap_or_default()
    }

    fn export(device: &Device, object: Object<'_, Self>) -> Result<Handle> {
        let raw = match object {
            Object::Buffer(b) => b.resource.as_raw(),
            Object::Texture(t) => t.resource.as_raw(),
            Object::Fence(f) => f.fence.as_raw(),
            Object::Heap(h) => h.heap.as_raw(),
            _ => return Err(invalid()),
        };
        let mut handle = ptr::null_mut();
        let d = &device.device;
        // SAFETY: valid interfaces and out pointer; default security, unnamed.
        check(unsafe {
            (d.vtbl().CreateSharedHandle)(
                d.as_raw(),
                raw,
                ptr::null(),
                GENERIC_ALL,
                ptr::null(),
                &mut handle,
            )
        })?;
        Ok(Handle(handle))
    }

    unsafe fn import_texture(
        device: &Device,
        handle: &Handle,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        let d = &device.device;
        // SAFETY: valid interface, handle, IID and out pointer.
        let resource = com::create::<ID3D12Resource>(|iid, out| unsafe {
            (d.vtbl().OpenSharedHandle)(d.as_raw(), handle.0, iid, out)
        })
        .map_err(error)?;
        Ok(resource::Texture::wrap(
            resource,
            desc,
            resource::resource_format(desc),
        ))
    }

    unsafe fn import_buffer(
        device: &Device,
        handle: &Handle,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        let d = &device.device;
        // SAFETY: valid interface, handle, IID and out pointer.
        let resource = com::create::<ID3D12Resource>(|iid, out| unsafe {
            (d.vtbl().OpenSharedHandle)(d.as_raw(), handle.0, iid, out)
        })
        .map_err(error)?;
        resource::wrap_buffer(resource, desc)
    }

    unsafe fn import_fence(device: &Device, handle: &Handle) -> Result<Fence> {
        let d = &device.device;
        // SAFETY: valid interface, handle, IID and out pointer.
        let fence = com::create::<ID3D12Fence>(|iid, out| unsafe {
            (d.vtbl().OpenSharedHandle)(d.as_raw(), handle.0, iid, out)
        })
        .map_err(error)?;
        Ok(Fence {
            fence,
            lost: device.lost.clone(),
        })
    }

    unsafe fn texture_from_native(
        _: &Device,
        raw: *mut c_void,
        desc: &TextureDesc,
    ) -> Result<resource::Texture> {
        // SAFETY: the caller guarantees a live ID3D12Resource; the wrapper adds its reference.
        let resource = unsafe { Com::<ID3D12Resource>::from_borrowed(raw) }.ok_or_else(invalid)?;
        Ok(resource::Texture::wrap(
            resource,
            desc,
            resource::resource_format(desc),
        ))
    }

    unsafe fn buffer_from_native(
        _: &Device,
        raw: *mut c_void,
        desc: &BufferDesc,
    ) -> Result<resource::Buffer> {
        // SAFETY: as above.
        let resource = unsafe { Com::<ID3D12Resource>::from_borrowed(raw) }.ok_or_else(invalid)?;
        resource::wrap_buffer(resource, desc)
    }
}

/// The raw D3D12 object behind `object`, or null for DXGI objects.
fn d3d12_object(object: &Object<'_, Backend>) -> *mut c_void {
    match *object {
        Object::Instance(_) | Object::Swapchain(_) => ptr::null_mut(),
        Object::Device(d) => d.device.as_raw(),
        Object::Queue(q) => q.queue.as_raw(),
        Object::Fence(f) => f.fence.as_raw(),
        Object::CommandPool(p) => p.as_raw(),
        Object::CommandList(l) => l.as_raw(),
        Object::Buffer(b) => b.resource.as_raw(),
        Object::Texture(t) => t.resource.as_raw(),
        Object::GraphicsPipeline(p) => p.pso.as_raw(),
        Object::ComputePipeline(p) => p.pso.as_raw(),
        Object::MeshPipeline(p) => p.pso.as_raw(),
        Object::QueryPool(p) => p.heap.as_raw(),
        Object::Heap(h) => h.heap.as_raw(),
        Object::AccelerationStructure(a) => a.resource.as_raw(),
    }
}

impl crate::contract::Handles for Backend {
    fn set_name(object: Object<'_, Self>, name: &str) {
        let raw = d3d12_object(&object);
        if !raw.is_null() {
            // SAFETY: a live D3D12 object.
            unsafe { diag::set_name(raw, name) };
        }
    }

    fn native(object: Object<'_, Self>) -> *mut c_void {
        match object {
            Object::Instance(i) => i.factory.as_raw(),
            Object::Swapchain(s) => s.as_raw(),
            other => d3d12_object(&other),
        }
    }

    fn shared_handle_raw(handle: &Handle) -> *mut c_void {
        handle.0
    }

    unsafe fn shared_handle_from_raw(raw: *mut c_void) -> Handle {
        Handle(raw)
    }
}

/// A query heap and the query type its queries are.
pub(crate) struct QueryPool {
    heap: Com<ID3D12QueryHeap>,
    query_type: i32,
}

impl crate::contract::Resources for Backend {
    fn buffer_mapped(buffer: &resource::Buffer) -> Option<NonNull<u8>> {
        buffer.mapped
    }

    fn shader_view_index(view: &Descriptor) -> u32 {
        view.index()
    }

    fn sampler_index(sampler: &Descriptor) -> u32 {
        sampler.index()
    }

    fn acceleration_structure_index(structure: &raytracing::AccelerationStructure) -> u32 {
        structure.view.index()
    }

    fn acceleration_structure_address(structure: &raytracing::AccelerationStructure) -> u64 {
        structure.address
    }
}

// --- Queue ---

/// Waits for everything submitted to a queue: signals a private fence and waits for it.
struct Idle {
    queue: Com<ID3D12CommandQueue>,
    fence: Fence,
    value: AtomicU64,
}

impl Idle {
    fn new(device: &Device, queue: Com<ID3D12CommandQueue>) -> Result<Self> {
        Ok(Self {
            queue,
            fence: create_fence(device, 0, false)?,
            value: AtomicU64::new(0),
        })
    }

    fn wait(&self) -> Result<()> {
        let value = self.value.fetch_add(1, Ordering::Relaxed) + 1;
        // SAFETY: valid interfaces.
        let hr = unsafe {
            (self.queue.vtbl().Signal)(self.queue.as_raw(), self.fence.fence.as_raw(), value)
        };
        self.fence.lost.result(hr)?;
        Backend::wait(&self.fence, value, None).map(drop)
    }
}

pub(crate) struct Queue {
    queue: Com<ID3D12CommandQueue>,
    idle: Idle,
    validation: Option<Arc<Validation>>,
    lost: Arc<Lost>,
}

impl Queue {
    fn new(device: &Device, ty: QueueType) -> Result<Self> {
        let desc = D3D12_COMMAND_QUEUE_DESC {
            Type: command::list_type(ty),
            Priority: 0,
            Flags: 0,
            NodeMask: 0,
        };
        let d = &device.device;
        // SAFETY: valid interface, description, IID and out pointer.
        let queue = com::create::<ID3D12CommandQueue>(|iid, out| unsafe {
            (d.vtbl().CreateCommandQueue)(d.as_raw(), &desc, iid, out)
        })
        .map_err(error)?;
        let name = match ty {
            QueueType::Graphics => "graphics queue",
            QueueType::Compute => "compute queue",
            QueueType::Copy => "copy queue",
        };
        // SAFETY: a live queue.
        unsafe { diag::set_name(queue.as_raw(), name) };
        Ok(Self {
            idle: Idle::new(device, queue.clone())?,
            queue,
            validation: device.validation.clone(),
            lost: device.lost.clone(),
        })
    }
}

impl crate::contract::Queue for Backend {
    unsafe fn submit(queue: &Queue, desc: &SubmitDesc<'_>) -> Result<()> {
        let q = &queue.queue;
        for (fence, value) in desc.waits {
            // SAFETY: valid interfaces; the wait is queued on the GPU, not the CPU.
            let hr = unsafe { (q.vtbl().Wait)(q.as_raw(), fence.raw.fence.as_raw(), *value) };
            queue.lost.result(hr)?;
        }
        for chunk in desc.lists.chunks(16) {
            let mut raw = [ptr::null_mut::<c_void>(); 16];
            for (r, list) in raw.iter_mut().zip(chunk) {
                *r = list.raw.as_raw();
            }
            // SAFETY: valid closed command lists of the queue's type (checked by `api.rs`); the
            // caller keeps their resources alive.
            unsafe { (q.vtbl().ExecuteCommandLists)(q.as_raw(), chunk.len() as u32, raw.as_ptr()) };
        }
        for (fence, value) in desc.signals {
            // SAFETY: valid interfaces.
            let hr = unsafe { (q.vtbl().Signal)(q.as_raw(), fence.raw.fence.as_raw(), *value) };
            queue.lost.result(hr)?;
        }
        flush(&queue.validation);
        Ok(())
    }

    fn timestamp_frequency(queue: &Queue) -> Result<u64> {
        let mut frequency = 0;
        let q = &queue.queue;
        // SAFETY: valid interface and out pointer.
        check(unsafe { (q.vtbl().GetTimestampFrequency)(q.as_raw(), &mut frequency) })?;
        Ok(frequency)
    }

    fn clock_calibration(queue: &Queue) -> Result<ClockCalibration> {
        let (mut gpu, mut cpu) = (0, 0);
        let q = &queue.queue;
        // SAFETY: valid interface and out pointers.
        let hr = unsafe { (q.vtbl().GetClockCalibration)(q.as_raw(), &mut gpu, &mut cpu) };
        queue.lost.result(hr)?;
        Ok(ClockCalibration { gpu, cpu })
    }

    fn wait_idle(queue: &Queue) -> Result<()> {
        queue.idle.wait()
    }

    unsafe fn bind_sparse(queue: &Queue, binds: &[SparseBind<'_>]) -> Result<()> {
        let q = &queue.queue;
        for bind in binds {
            let (resource, coordinate, size, memory) = match *bind {
                SparseBind::Buffer {
                    buffer,
                    tile_offset,
                    tile_count,
                    memory,
                } => (
                    buffer.raw.resource.as_raw(),
                    D3D12_TILED_RESOURCE_COORDINATE {
                        X: tile_offset,
                        ..Default::default()
                    },
                    D3D12_TILE_REGION_SIZE {
                        NumTiles: tile_count,
                        ..Default::default()
                    },
                    memory,
                ),
                SparseBind::Texture {
                    texture,
                    region,
                    memory,
                } => {
                    let t = &texture.raw;
                    let layout = t.sparse.ok_or_else(invalid)?;
                    let (coordinate, size) = match region {
                        SparseTextureRegion::Tiles {
                            mip,
                            layer,
                            origin,
                            size,
                        } => {
                            if mip >= layout.standard_mips {
                                return Err(invalid());
                            }
                            (
                                D3D12_TILED_RESOURCE_COORDINATE {
                                    X: origin[0],
                                    Y: origin[1],
                                    Z: origin[2],
                                    Subresource: mip + layer * t.mip_levels,
                                },
                                D3D12_TILE_REGION_SIZE {
                                    NumTiles: size[0] * size[1] * size[2],
                                    UseBox: 1,
                                    Width: size[0],
                                    Height: size[1] as u16,
                                    Depth: size[2] as u16,
                                },
                            )
                        }
                        SparseTextureRegion::PackedMips { layer } => {
                            if layout.packed_mip_tiles == 0 {
                                return Err(invalid());
                            }
                            (
                                D3D12_TILED_RESOURCE_COORDINATE {
                                    Subresource: layout.standard_mips + layer * t.mip_levels,
                                    ..Default::default()
                                },
                                D3D12_TILE_REGION_SIZE {
                                    NumTiles: layout.packed_mip_tiles,
                                    ..Default::default()
                                },
                            )
                        }
                    };
                    (t.resource.as_raw(), coordinate, size, memory)
                }
            };
            let (heap, flags, start) = match memory {
                Some((heap, first)) => (heap.raw.heap.as_raw(), 0, first),
                None => (ptr::null_mut(), D3D12_TILE_RANGE_FLAG_NULL, 0),
            };
            let count = size.NumTiles;
            // SAFETY: valid interfaces; one region and one range, checked by `api.rs` and above;
            // the caller keeps the heap alive while bound.
            unsafe {
                (q.vtbl().UpdateTileMappings)(
                    q.as_raw(),
                    resource,
                    1,
                    &coordinate,
                    &size,
                    heap,
                    1,
                    &flags,
                    &start,
                    &count,
                    0,
                )
            };
        }
        flush(&queue.validation);
        Ok(())
    }
}

// --- Fence ---

pub(crate) struct Fence {
    fence: Com<ID3D12Fence>,
    lost: Arc<Lost>,
}

impl crate::contract::Fences for Backend {
    fn completed_value(fence: &Fence) -> u64 {
        // SAFETY: valid interface.
        unsafe { (fence.fence.vtbl().GetCompletedValue)(fence.fence.as_raw()) }
    }

    fn wait(fence: &Fence, value: u64, timeout: Option<Duration>) -> Result<bool> {
        if Self::completed_value(fence) < value {
            // One event per wait: concurrent waiters on a shared auto-reset event could steal
            // each other's wake-up. A blocking wait dwarfs the cost.
            let event = event()?;
            let f = &fence.fence;
            // SAFETY: valid interface and event.
            let hr = unsafe { (f.vtbl().SetEventOnCompletion)(f.as_raw(), value, event.0) };
            fence.lost.result(hr)?;
            if !wait_handle(event.0, timeout)? {
                return Ok(false);
            }
        }
        // A removed device completes its fences at u64::MAX, which no frame loop reaches.
        if Self::completed_value(fence) == u64::MAX {
            fence.lost.check()?;
        }
        Ok(true)
    }

    fn signal(fence: &Fence, value: u64) -> Result<()> {
        // SAFETY: valid interface.
        let hr = unsafe { (fence.fence.vtbl().Signal)(fence.fence.as_raw(), value) };
        fence.lost.result(hr)
    }
}
