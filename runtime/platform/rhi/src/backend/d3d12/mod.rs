//! D3D12 backend (Windows; the Xbox backend builds on it).
//!
//! DXGI and D3D12 are loaded from System32 at run time, so a machine without them gets
//! `Unsupported` instead of a loader error. Baseline: feature level 12_0. Enhanced barriers are
//! used when the runtime and driver report them (`D3D12_OPTIONS12`); otherwise barriers become
//! legacy resource-state transitions.

mod com;
mod command;
mod ffi;
mod pipeline;
mod resource;
mod swapchain;

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use core::time::Duration;

use alloc::sync::Arc;
use alloc::vec;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows_sys::core::{BOOL, HRESULT};

use com::*;
use ffi::*;

use core::ptr::NonNull;

use crate::contract::{Fences, Queues, Rhi, Types};
use crate::{
    AdapterInfo, AdapterKind, AdapterPreference, Buffer as PublicBuffer, BufferDesc,
    BufferViewDesc, ComputePipelineDesc, DepthStencilViewDesc, DeviceDesc, Error, ErrorKind,
    Features, GraphicsPipelineDesc, HeapDesc, InstanceDesc, MemoryRequirements, MeshPipelineDesc,
    QueryKind, QueryPoolDesc, QueueType, RenderTargetViewDesc, Result, SamplerDesc, SubmitDesc,
    SurfaceHandle, SwapchainDesc, Texture as PublicTexture, TextureDesc, TextureViewDesc,
};
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

/// A kernel handle, closed on drop.
struct Handle(HANDLE);

// SAFETY: kernel handles are process-wide.
unsafe impl Send for Handle {}
// SAFETY: as above; waiting on a handle from several threads is allowed.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Waits on `handle` for up to `timeout`. `Ok(false)` on timeout.
fn wait_handle(handle: HANDLE, timeout: Option<Duration>) -> Result<bool> {
    let ms = timeout.map_or(INFINITE, |t| t.as_millis().min(INFINITE as u128 - 1) as u32);
    // SAFETY: `handle` is a live waitable handle.
    match unsafe { WaitForSingleObject(handle, ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(Error::from_kind(ErrorKind::Other)),
    }
}

// --- Validation ---

/// 0 = not tried, 1 = enabling, 2 = done (enabled or unavailable).
static DEBUG_LAYER: AtomicU8 = AtomicU8::new(0);

/// Enables the debug layer once per process, before any device that should be validated. Other
/// threads wait for the attempt: a device created while it is being enabled would be removed.
fn enable_debug_layer() {
    loop {
        match DEBUG_LAYER.compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => break,
            Err(2) => return,
            Err(_) => core::hint::spin_loop(),
        }
    }
    if let Some(get) = d3d12_get_debug_interface() {
        // SAFETY: valid IID and out pointer.
        match com::create::<ID3D12Debug>(|iid, out| unsafe { get(iid, out) }) {
            // SAFETY: valid interface.
            Ok(debug) => unsafe { (debug.vtbl().EnableDebugLayer)(debug.as_raw()) },
            Err(_) => marlin_diag::warn!(
                "D3D12 debug layer unavailable (install the Graphics Tools optional feature)"
            ),
        }
    }
    DEBUG_LAYER.store(2, Ordering::Release);
}

/// The debug layer's message queue, drained into the `marlin_diag` log after submits and
/// presents.
struct Validation {
    info_queue: Com<ID3D12InfoQueue>,
    /// The info queue is not documented as thread-safe.
    lock: AtomicBool,
    errors: AtomicU64,
}

impl Validation {
    fn flush(&self) {
        if self.lock.swap(true, Ordering::Acquire) {
            return; // Another thread is draining.
        }
        let q = &self.info_queue;
        // SAFETY: valid interface.
        let count = unsafe { (q.vtbl().GetNumStoredMessagesAllowedByRetrievalFilter)(q.as_raw()) };
        for i in 0..count {
            let mut len = 0;
            // SAFETY: a null message pointer queries the size.
            if unsafe { (q.vtbl().GetMessage)(q.as_raw(), i, ptr::null_mut(), &mut len) } < 0 {
                continue;
            }
            // u64 words keep D3D12_MESSAGE aligned.
            let mut buf = vec![0u64; len.div_ceil(8)];
            let message = buf.as_mut_ptr().cast::<D3D12_MESSAGE>();
            // SAFETY: `buf` holds `len` bytes.
            if unsafe { (q.vtbl().GetMessage)(q.as_raw(), i, message, &mut len) } < 0 {
                continue;
            }
            // SAFETY: GetMessage filled the header; the description it points to lives in `buf`
            // and is NUL-terminated (`DescriptionByteLength` includes the NUL).
            let (severity, text) = unsafe {
                let m = &*message;
                let bytes = core::slice::from_raw_parts(
                    m.pDescription,
                    m.DescriptionByteLength.saturating_sub(1),
                );
                (
                    m.Severity,
                    core::str::from_utf8(bytes).unwrap_or("<invalid UTF-8>"),
                )
            };
            match severity {
                D3D12_MESSAGE_SEVERITY_CORRUPTION | D3D12_MESSAGE_SEVERITY_ERROR => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    marlin_diag::error!("D3D12: {text}");
                }
                D3D12_MESSAGE_SEVERITY_WARNING => marlin_diag::warn!("D3D12: {text}"),
                _ => marlin_diag::debug!("D3D12: {text}"),
            }
        }
        // SAFETY: valid interface.
        unsafe { (q.vtbl().ClearStoredMessages)(q.as_raw()) };
        self.lock.store(false, Ordering::Release);
    }
}

fn flush(validation: &Option<Arc<Validation>>) {
    if let Some(v) = validation {
        v.flush();
    }
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

fn adapter_info(adapter: &Com<IDXGIAdapter1>, index: u32) -> Option<AdapterInfo> {
    // SAFETY: all-zero is a valid DXGI_ADAPTER_DESC1.
    let mut desc: DXGI_ADAPTER_DESC1 = unsafe { core::mem::zeroed() };
    // SAFETY: valid interface and out pointer.
    if unsafe { (adapter.vtbl().GetDesc1)(adapter.as_raw(), &mut desc) } < 0 {
        return None;
    }
    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
    let mut name = [0u8; 256];
    let mut n = 0;
    for c in char::decode_utf16(desc.Description[..len].iter().copied()) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        if n + c.len_utf8() > name.len() {
            break;
        }
        n += c.encode_utf8(&mut name[n..]).len();
    }
    let kind = if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0 {
        AdapterKind::Software
    } else {
        AdapterKind::Hardware
    };
    let mut info = AdapterInfo::new(
        index,
        kind,
        core::str::from_utf8(&name[..n]).unwrap_or_default(),
    );
    info.vendor_id = desc.VendorId;
    info.device_id = desc.DeviceId;
    info.dedicated_video_memory = desc.DedicatedVideoMemory as u64;
    Some(info)
}

/// Queries a feature-support struct; `false` if the runtime does not know the feature.
///
/// # Safety
/// `T` is the struct `feature` reports.
unsafe fn feature<T>(device: &Com<ID3D12Device>, feature: i32, data: &mut T) -> bool {
    // SAFETY: valid interface; `data` matches the feature (caller).
    let hr = unsafe {
        (device.vtbl().CheckFeatureSupport)(
            device.as_raw(),
            feature,
            (data as *mut T).cast(),
            size_of::<T>() as u32,
        )
    };
    hr >= 0
}

/// The highest shader model the device supports, as `0xMm`.
fn shader_model(device: &Com<ID3D12Device>) -> i32 {
    // Runtimes reject models newer than they know: step down until one is accepted.
    (D3D_SHADER_MODEL_6_6..=D3D_SHADER_MODEL_HIGHEST)
        .rev()
        .find_map(|requested| {
            let mut data = D3D12_FEATURE_DATA_SHADER_MODEL {
                HighestShaderModel: requested,
            };
            // SAFETY: the struct matches the feature.
            unsafe { feature(device, D3D12_FEATURE_SHADER_MODEL, &mut data) }
                .then_some(data.HighestShaderModel)
        })
        .unwrap_or(0)
}

/// Creates a device on `adapter` if it meets the baseline: feature level 12_0, shader model 6.6,
/// resource binding tier 3 (bindless through directly indexed heaps), mesh shader tier 1 and wave
/// intrinsics.
fn create_d3d12_device(adapter: &Com<IDXGIAdapter1>) -> Result<(Com<ID3D12Device>, i32)> {
    let create = d3d12_create_device().ok_or_else(unsupported)?;
    // SAFETY: valid adapter, IID and out pointer.
    let device = com::create::<ID3D12Device>(|iid, out| unsafe {
        create(adapter.as_raw(), D3D_FEATURE_LEVEL_12_0, iid, out)
    })
    .map_err(error)?;
    let model = shader_model(&device);
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    // SAFETY: the struct matches the feature.
    let tier = match unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS, &mut options) } {
        true => options.ResourceBindingTier,
        false => 0,
    };
    let mut options1 = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: the struct matches the feature.
    unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS1, &mut options1) };
    let mut options7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
    // SAFETY: the struct matches the feature. Older runtimes reject it, leaving tier 0.
    unsafe { feature(&device, D3D12_FEATURE_D3D12_OPTIONS7, &mut options7) };
    if model < D3D_SHADER_MODEL_6_6
        || tier < D3D12_RESOURCE_BINDING_TIER_3
        || options7.MeshShaderTier == 0
        || options1.WaveOps == 0
    {
        marlin_diag::warn!(
            "adapter lacks the D3D12 baseline: shader model {:#x}, resource binding tier {tier}, \
             mesh shader tier {}, wave ops {}",
            model,
            options7.MeshShaderTier,
            options1.WaveOps != 0
        );
        return Err(unsupported());
    }
    Ok((device, model))
}

impl crate::contract::Instance for Backend {
    fn create_instance(desc: &InstanceDesc) -> Result<Instance> {
        if desc.validation {
            enable_debug_layer();
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

    fn create_device(instance: &Instance, desc: &DeviceDesc) -> Result<(Device, Queues<Queue>)> {
        let (adapter, device, index) = match desc.adapter {
            AdapterPreference::HighPerformance => {
                let mut found = None;
                for index in 0.. {
                    let Some(adapter) = instance.enum_adapter(index) else {
                        break;
                    };
                    let hardware = adapter_info(&adapter, index)
                        .is_some_and(|info| info.kind == AdapterKind::Hardware);
                    if let (true, Ok(device)) = (hardware, create_d3d12_device(&adapter)) {
                        found = Some((adapter, device, index));
                        break;
                    }
                }
                found.ok_or(Error::from_kind(ErrorKind::NotFound))?
            }
            AdapterPreference::Software => {
                let adapter = instance.warp_adapter()?;
                let device = create_d3d12_device(&adapter)?;
                // Not in the enumeration order; report it past the end.
                let index = (0..)
                    .find(|&i| instance.enum_adapter(i).is_none())
                    .unwrap_or(0);
                (adapter, device, index)
            }
            AdapterPreference::Index(index) => {
                let adapter = instance
                    .enum_adapter(index)
                    .ok_or(Error::from_kind(ErrorKind::NotFound))?;
                let device = create_d3d12_device(&adapter)?;
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

        let features = query_features(&device, model, instance.tearing, desc);
        let device2 = device.cast::<ID3D12Device2>().map_err(error)?;

        let device = Device {
            // SAFETY: valid interface.
            rtv_size: unsafe {
                (device.vtbl().GetDescriptorHandleIncrementSize)(
                    device.as_raw(),
                    D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                )
            },
            bindless: Bindless::new(&device)?,
            device,
            device2,
            factory: instance.factory.clone(),
            info,
            features,
            validation,
        };
        let queues = Queues {
            graphics: Queue::new(&device, QueueType::Graphics)?,
            compute: Some(Queue::new(&device, QueueType::Compute)?),
            copy: Some(Queue::new(&device, QueueType::Copy)?),
        };
        Ok((device, queues))
    }
}

/// The device's optional features (the baseline was checked by [`create_d3d12_device`]).
fn query_features(
    device: &Com<ID3D12Device>,
    model: i32,
    tearing: bool,
    desc: &DeviceDesc,
) -> Features {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    // SAFETY: each struct below matches its feature. Older runtimes reject newer features,
    // leaving them zeroed (unsupported).
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS, &mut options) };
    let mut options1 = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS1, &mut options1) };
    let mut options3 = D3D12_FEATURE_DATA_D3D12_OPTIONS3::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS3, &mut options3) };
    let mut options5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS5, &mut options5) };
    let mut options7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS7, &mut options7) };
    let mut options9 = D3D12_FEATURE_DATA_D3D12_OPTIONS9::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS9, &mut options9) };
    let mut options12 = D3D12_FEATURE_DATA_D3D12_OPTIONS12::default();
    // SAFETY: as above.
    unsafe { feature(device, D3D12_FEATURE_D3D12_OPTIONS12, &mut options12) };
    Features {
        shader_model: ((model >> 4) as u8, (model & 0xf) as u8),
        enhanced_barriers: options12.EnhancedBarriersSupported != 0
            && !desc.disable_enhanced_barriers,
        tearing,
        mesh_shader: options7.MeshShaderTier != 0,
        ray_query: options5.RaytracingTier >= D3D12_RAYTRACING_TIER_1_1,
        atomic_int64: options9.AtomicInt64OnTypedResourceSupported != 0
            && options9.AtomicInt64OnGroupSharedSupported != 0,
        copy_queue_timestamps: options3.CopyQueueTimestampQueriesSupported != 0,
        mixed_heaps: options.ResourceHeapTier >= D3D12_RESOURCE_HEAP_TIER_2,
        wave_size_range: (options1.WaveLaneCountMin, options1.WaveLaneCountMax),
    }
}

// --- Device ---

pub(crate) struct Device {
    device: Com<ID3D12Device>,
    /// Pipeline state streams (mesh pipelines).
    device2: Com<ID3D12Device2>,
    factory: Com<IDXGIFactory4>,
    info: AdapterInfo,
    features: Features,
    rtv_size: u32,
    bindless: Arc<Bindless>,
    validation: Option<Arc<Validation>>,
}

impl Drop for Device {
    fn drop(&mut self) {
        flush(&self.validation);
    }
}

fn create_fence(device: &Com<ID3D12Device>, initial: u64) -> Result<Fence> {
    // SAFETY: valid interface, IID and out pointer.
    let fence = com::create::<ID3D12Fence>(|iid, out| unsafe {
        (device.vtbl().CreateFence)(device.as_raw(), initial, 0, iid, out)
    })
    .map_err(error)?;
    Ok(Fence { fence })
}

impl crate::contract::Device for Backend {
    fn adapter_info(device: &Device) -> &AdapterInfo {
        &device.info
    }

    fn features(device: &Device) -> Features {
        device.features
    }

    fn create_fence(device: &Device, initial: u64) -> Result<Fence> {
        create_fence(&device.device, initial)
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
        surface: SurfaceHandle,
        desc: &SwapchainDesc,
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
        texture: &PublicTexture,
        desc: &RenderTargetViewDesc,
    ) -> Result<resource::RenderTargetView> {
        resource::create_render_target_view(device, texture, desc)
    }

    fn create_depth_stencil_view(
        device: &Device,
        texture: &PublicTexture,
        desc: &DepthStencilViewDesc,
    ) -> Result<resource::DepthStencilView> {
        resource::create_depth_stencil_view(device, texture, desc)
    }

    fn create_texture_view(
        device: &Device,
        texture: &PublicTexture,
        desc: &TextureViewDesc,
    ) -> Result<Descriptor> {
        resource::create_texture_view(device, texture, desc)
    }

    fn create_buffer_view(
        device: &Device,
        buffer: &PublicBuffer,
        desc: &BufferViewDesc,
    ) -> Result<Descriptor> {
        resource::create_buffer_view(device, buffer, desc)
    }

    fn create_sampler(device: &Device, desc: &SamplerDesc) -> Result<Descriptor> {
        resource::create_sampler(device, desc)
    }

    fn create_graphics_pipeline(
        device: &Device,
        desc: &GraphicsPipelineDesc<'_>,
    ) -> Result<pipeline::GraphicsPipeline> {
        let pipeline = pipeline::create_graphics_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_compute_pipeline(
        device: &Device,
        desc: &ComputePipelineDesc<'_>,
    ) -> Result<pipeline::ComputePipeline> {
        let pipeline = pipeline::create_compute_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_mesh_pipeline(
        device: &Device,
        desc: &MeshPipelineDesc<'_>,
    ) -> Result<pipeline::MeshPipeline> {
        let pipeline = pipeline::create_mesh_pipeline(device, desc);
        flush(&device.validation);
        pipeline
    }

    fn create_query_pool(device: &Device, desc: &QueryPoolDesc) -> Result<QueryPool> {
        let heap_desc = D3D12_QUERY_HEAP_DESC {
            Type: match desc.kind {
                QueryKind::Timestamp => D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                QueryKind::CopyTimestamp => D3D12_QUERY_HEAP_TYPE_COPY_QUEUE_TIMESTAMP,
            },
            Count: desc.count,
            NodeMask: 0,
        };
        let d = &device.device;
        // SAFETY: valid interface, description, IID and out pointer.
        let heap = com::create(|iid, out| unsafe {
            (d.vtbl().CreateQueryHeap)(d.as_raw(), &heap_desc, iid, out)
        })
        .map_err(error)?;
        Ok(QueryPool { heap })
    }

    fn create_heap(device: &Device, desc: &HeapDesc) -> Result<resource::Heap> {
        resource::create_heap(device, desc)
    }

    fn buffer_requirements(device: &Device, desc: &BufferDesc) -> MemoryRequirements {
        resource::buffer_requirements(device, desc)
    }

    fn texture_requirements(device: &Device, desc: &TextureDesc) -> MemoryRequirements {
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
}

/// A timestamp query heap (of the copy queue's type for [`QueryKind::CopyTimestamp`]).
pub(crate) struct QueryPool {
    heap: Com<ID3D12QueryHeap>,
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
            fence: create_fence(&device.device, 0)?,
            value: AtomicU64::new(0),
        })
    }

    fn wait(&self) -> Result<()> {
        let value = self.value.fetch_add(1, Ordering::Relaxed) + 1;
        // SAFETY: valid interfaces.
        check(unsafe {
            (self.queue.vtbl().Signal)(self.queue.as_raw(), self.fence.fence.as_raw(), value)
        })?;
        Backend::wait(&self.fence, value, None).map(drop)
    }
}

pub(crate) struct Queue {
    queue: Com<ID3D12CommandQueue>,
    idle: Idle,
    validation: Option<Arc<Validation>>,
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
        Ok(Self {
            idle: Idle::new(device, queue.clone())?,
            queue,
            validation: device.validation.clone(),
        })
    }
}

impl crate::contract::Queue for Backend {
    unsafe fn submit(queue: &Queue, desc: &SubmitDesc<'_>) -> Result<()> {
        let q = &queue.queue;
        for (fence, value) in desc.waits {
            // SAFETY: valid interfaces; the wait is queued on the GPU, not the CPU.
            check(unsafe { (q.vtbl().Wait)(q.as_raw(), fence.raw.fence.as_raw(), *value) })?;
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
            check(unsafe { (q.vtbl().Signal)(q.as_raw(), fence.raw.fence.as_raw(), *value) })?;
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

    fn wait_idle(queue: &Queue) -> Result<()> {
        queue.idle.wait()
    }
}

// --- Fence ---

pub(crate) struct Fence {
    fence: Com<ID3D12Fence>,
}

impl crate::contract::Fences for Backend {
    fn completed_value(fence: &Fence) -> u64 {
        // SAFETY: valid interface.
        unsafe { (fence.fence.vtbl().GetCompletedValue)(fence.fence.as_raw()) }
    }

    fn wait(fence: &Fence, value: u64, timeout: Option<Duration>) -> Result<bool> {
        if Self::completed_value(fence) >= value {
            return Ok(true);
        }
        // One event per wait: concurrent waiters on a shared auto-reset event could steal each
        // other's wake-up. A blocking wait dwarfs the cost.
        // SAFETY: unnamed auto-reset event with default security.
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if event.is_null() {
            return Err(Error::from_kind(ErrorKind::OutOfMemory));
        }
        let event = Handle(event);
        let f = &fence.fence;
        // SAFETY: valid interface and event.
        check(unsafe { (f.vtbl().SetEventOnCompletion)(f.as_raw(), value, event.0) })?;
        wait_handle(event.0, timeout)
    }

    fn signal(fence: &Fence, value: u64) -> Result<()> {
        // SAFETY: valid interface.
        check(unsafe { (fence.fence.vtbl().Signal)(fence.fence.as_raw(), value) })
    }
}
