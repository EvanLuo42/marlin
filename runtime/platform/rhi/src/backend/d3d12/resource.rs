//! Buffers, textures, heaps, views, samplers and the descriptor heaps behind them.
//!
//! Bindless: buffer and texture views live in one shader-visible CBV/SRV/UAV heap, samplers in
//! one shader-visible sampler heap, and a view's slot is its shader-side index (SM 6.6
//! `ResourceDescriptorHeap[i]` / `SamplerDescriptorHeap[i]`). Render target, depth and clear views
//! live in CPU-only heaps that grow by chunks. Every slot is returned to its heap's free list when
//! its owner drops.
//!
//! Resources are committed (their own implicit heap), placed in a [`Heap`] at an offset (placed
//! resources keep their heap alive), or reserved (sparse: address space only, tiles bound later).

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::com::{self, *};
use super::ffi::*;
use super::{Device, check, error, invalid, unsupported};
use crate::{
    AddressMode, BorderColor, Buffer as PublicBuffer, BufferDesc, BufferUsage, BufferViewDesc,
    BufferViewKind, ClearValue, CompareOp, DepthStencilViewDesc, Error, ErrorKind, Filter, Format,
    HeapDesc, HeapKind, MemoryLocation, MemoryRequirements, RenderTargetViewDesc, Result,
    SamplerDesc, SamplerReduction, SparseTextureLayout, Texture as PublicTexture, TextureDesc,
    TextureDimension, TextureUsage, TextureViewDesc, TextureViewDimension, UNIFORM_ALIGNMENT,
};

/// The DXGI format of `format`, or `DXGI_FORMAT_UNKNOWN` for formats D3D12 lacks (ETC2, EAC,
/// ASTC).
pub(super) fn dxgi_format(format: Format) -> u32 {
    use Format::*;
    match format {
        R8Unorm => DXGI_FORMAT_R8_UNORM,
        R8Snorm => DXGI_FORMAT_R8_SNORM,
        R8Uint => DXGI_FORMAT_R8_UINT,
        R8Sint => DXGI_FORMAT_R8_SINT,
        Rg8Unorm => DXGI_FORMAT_R8G8_UNORM,
        Rg8Snorm => DXGI_FORMAT_R8G8_SNORM,
        Rg8Uint => DXGI_FORMAT_R8G8_UINT,
        Rg8Sint => DXGI_FORMAT_R8G8_SINT,
        Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        Rgba8UnormSrgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        Rgba8Snorm => DXGI_FORMAT_R8G8B8A8_SNORM,
        Rgba8Uint => DXGI_FORMAT_R8G8B8A8_UINT,
        Rgba8Sint => DXGI_FORMAT_R8G8B8A8_SINT,
        Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        Bgra8UnormSrgb => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        Rgb10a2Unorm => DXGI_FORMAT_R10G10B10A2_UNORM,
        Rgb10a2Uint => DXGI_FORMAT_R10G10B10A2_UINT,
        Rg11b10Float => DXGI_FORMAT_R11G11B10_FLOAT,
        Rgb9e5Ufloat => DXGI_FORMAT_R9G9B9E5_SHAREDEXP,
        R16Unorm => DXGI_FORMAT_R16_UNORM,
        R16Snorm => DXGI_FORMAT_R16_SNORM,
        R16Uint => DXGI_FORMAT_R16_UINT,
        R16Sint => DXGI_FORMAT_R16_SINT,
        R16Float => DXGI_FORMAT_R16_FLOAT,
        Rg16Unorm => DXGI_FORMAT_R16G16_UNORM,
        Rg16Snorm => DXGI_FORMAT_R16G16_SNORM,
        Rg16Uint => DXGI_FORMAT_R16G16_UINT,
        Rg16Sint => DXGI_FORMAT_R16G16_SINT,
        Rg16Float => DXGI_FORMAT_R16G16_FLOAT,
        Rgba16Unorm => DXGI_FORMAT_R16G16B16A16_UNORM,
        Rgba16Snorm => DXGI_FORMAT_R16G16B16A16_SNORM,
        Rgba16Uint => DXGI_FORMAT_R16G16B16A16_UINT,
        Rgba16Sint => DXGI_FORMAT_R16G16B16A16_SINT,
        Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        R32Uint => DXGI_FORMAT_R32_UINT,
        R32Sint => DXGI_FORMAT_R32_SINT,
        R32Float => DXGI_FORMAT_R32_FLOAT,
        Rg32Uint => DXGI_FORMAT_R32G32_UINT,
        Rg32Sint => DXGI_FORMAT_R32G32_SINT,
        Rg32Float => DXGI_FORMAT_R32G32_FLOAT,
        Rgba32Uint => DXGI_FORMAT_R32G32B32A32_UINT,
        Rgba32Sint => DXGI_FORMAT_R32G32B32A32_SINT,
        Rgba32Float => DXGI_FORMAT_R32G32B32A32_FLOAT,
        D16Unorm => DXGI_FORMAT_D16_UNORM,
        D32Float => DXGI_FORMAT_D32_FLOAT,
        D32FloatS8 => DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
        Bc1Unorm => DXGI_FORMAT_BC1_UNORM,
        Bc1UnormSrgb => DXGI_FORMAT_BC1_UNORM_SRGB,
        Bc2Unorm => DXGI_FORMAT_BC2_UNORM,
        Bc2UnormSrgb => DXGI_FORMAT_BC2_UNORM_SRGB,
        Bc3Unorm => DXGI_FORMAT_BC3_UNORM,
        Bc3UnormSrgb => DXGI_FORMAT_BC3_UNORM_SRGB,
        Bc4Unorm => DXGI_FORMAT_BC4_UNORM,
        Bc4Snorm => DXGI_FORMAT_BC4_SNORM,
        Bc5Unorm => DXGI_FORMAT_BC5_UNORM,
        Bc5Snorm => DXGI_FORMAT_BC5_SNORM,
        Bc6hUfloat => DXGI_FORMAT_BC6H_UF16,
        Bc6hSfloat => DXGI_FORMAT_BC6H_SF16,
        Bc7Unorm => DXGI_FORMAT_BC7_UNORM,
        Bc7UnormSrgb => DXGI_FORMAT_BC7_UNORM_SRGB,
        _ => DXGI_FORMAT_UNKNOWN,
    }
}

/// [`dxgi_format`], failing with `Unsupported` for formats D3D12 lacks.
fn known_format(format: Format) -> Result<u32> {
    match dxgi_format(format) {
        DXGI_FORMAT_UNKNOWN => Err(unsupported()),
        f => Ok(f),
    }
}

// --- Descriptor heaps ---

pub(super) struct DescriptorHeap {
    heap: Com<ID3D12DescriptorHeap>,
    start: usize,
    /// The shader-visible heaps' GPU address of slot 0 (0 for CPU-only heaps).
    gpu_start: u64,
    increment: usize,
    capacity: u32,
    free: Mutex<FreeList>,
}

struct FreeList {
    /// Slots below this were handed out at least once.
    next: u32,
    freed: Vec<u32>,
}

impl DescriptorHeap {
    pub(super) fn new(
        device: &Com<ID3D12Device>,
        ty: i32,
        capacity: u32,
        shader_visible: bool,
    ) -> Result<Arc<Self>> {
        let desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: ty,
            NumDescriptors: capacity,
            Flags: match shader_visible {
                true => D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                false => 0,
            },
            NodeMask: 0,
        };
        // SAFETY: valid interface, description, IID and out pointer.
        let heap = com::create::<ID3D12DescriptorHeap>(|iid, out| unsafe {
            (device.vtbl().CreateDescriptorHeap)(device.as_raw(), &desc, iid, out)
        })
        .map_err(error)?;
        let mut start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        // SAFETY: valid interface; the struct is returned through `start`.
        unsafe { (heap.vtbl().GetCPUDescriptorHandleForHeapStart)(heap.as_raw(), &mut start) };
        let mut gpu_start = D3D12_GPU_DESCRIPTOR_HANDLE::default();
        if shader_visible {
            // SAFETY: valid shader-visible heap; returned through `gpu_start`.
            unsafe {
                (heap.vtbl().GetGPUDescriptorHandleForHeapStart)(heap.as_raw(), &mut gpu_start)
            };
        }
        // SAFETY: valid interface.
        let increment =
            unsafe { (device.vtbl().GetDescriptorHandleIncrementSize)(device.as_raw(), ty) };
        Ok(Arc::new(Self {
            heap,
            start: start.ptr,
            gpu_start: gpu_start.ptr,
            increment: increment as usize,
            capacity,
            free: Mutex::new(FreeList {
                next: 0,
                freed: Vec::new(),
            }),
        }))
    }

    pub(super) fn as_raw(&self) -> *mut c_void {
        self.heap.as_raw()
    }

    fn take(self: &Arc<Self>) -> Option<Descriptor> {
        let mut list = self.free.lock();
        let index = match list.freed.pop() {
            Some(index) => index,
            None if list.next < self.capacity => {
                list.next += 1;
                list.next - 1
            }
            None => return None,
        };
        Some(Descriptor {
            heap: self.clone(),
            index,
        })
    }
}

/// CPU-only descriptor heaps of one type that grow a chunk at a time: render target, depth and
/// clear views have no shader-side index, so they need not share one heap.
pub(super) struct CpuDescriptors {
    device: Com<ID3D12Device>,
    ty: i32,
    chunk: u32,
    heaps: Mutex<Vec<Arc<DescriptorHeap>>>,
}

impl CpuDescriptors {
    pub(super) fn new(device: &Com<ID3D12Device>, ty: i32, chunk: u32) -> Self {
        Self {
            device: device.clone(),
            ty,
            chunk,
            heaps: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn take(&self) -> Result<Descriptor> {
        let mut heaps = self.heaps.lock();
        // The newest chunk first: older ones only have freed slots.
        for heap in heaps.iter().rev() {
            if let Some(slot) = heap.take() {
                return Ok(slot);
            }
        }
        let heap = DescriptorHeap::new(&self.device, self.ty, self.chunk, false)?;
        let slot = heap
            .take()
            .ok_or(Error::from_kind(ErrorKind::OutOfMemory))?;
        heaps.push(heap);
        Ok(slot)
    }
}

/// One slot of a descriptor heap, returned to it on drop.
pub(crate) struct Descriptor {
    heap: Arc<DescriptorHeap>,
    index: u32,
}

impl Descriptor {
    /// A slot of a fixed-size (shader-visible) heap; `OutOfMemory` when it is full.
    pub(super) fn new(heap: &Arc<DescriptorHeap>) -> Result<Self> {
        heap.take().ok_or(Error::from_kind(ErrorKind::OutOfMemory))
    }

    pub(super) fn handle(&self) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.heap.start + self.index as usize * self.heap.increment,
        }
    }

    /// The slot's GPU handle; shader-visible heaps only.
    pub(super) fn gpu_handle(&self) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.heap.gpu_start + self.index as u64 * self.heap.increment as u64,
        }
    }

    pub(super) fn index(&self) -> u32 {
        self.index
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        self.heap.free.lock().freed.push(self.index);
    }
}

// --- Heaps ---

pub(crate) struct Heap {
    pub(super) heap: Com<ID3D12Heap>,
}

fn heap_type(memory: MemoryLocation) -> i32 {
    match memory {
        MemoryLocation::GpuOnly => D3D12_HEAP_TYPE_DEFAULT,
        MemoryLocation::Upload => D3D12_HEAP_TYPE_UPLOAD,
        MemoryLocation::Readback => D3D12_HEAP_TYPE_READBACK,
        MemoryLocation::GpuUpload => D3D12_HEAP_TYPE_GPU_UPLOAD,
    }
}

pub(super) fn create_heap(device: &Device, desc: &HeapDesc) -> Result<Heap> {
    let kind = match desc.kind {
        // Tier 2 mixes everything (`api.rs` checks the tier).
        HeapKind::Any => 0,
        HeapKind::Buffers => D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS,
        HeapKind::Textures => D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES,
        HeapKind::Targets => D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES,
    };
    // Multisampled textures need 4 MiB placement; heaps that may hold them get it.
    let alignment = match desc.kind {
        HeapKind::Any | HeapKind::Targets => crate::MSAA_PLACEMENT_ALIGNMENT,
        _ => D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT,
    };
    let heap_desc = D3D12_HEAP_DESC {
        SizeInBytes: desc.size.next_multiple_of(alignment),
        Properties: heap_properties(heap_type(desc.memory)),
        Alignment: alignment,
        Flags: kind | D3D12_HEAP_FLAG_CREATE_NOT_ZEROED,
    };
    let d = &device.device;
    // SAFETY: valid interface, description, IID and out pointer.
    let heap =
        com::create(|iid, out| unsafe { (d.vtbl().CreateHeap)(d.as_raw(), &heap_desc, iid, out) })
            .map_err(error)?;
    Ok(Heap { heap })
}

fn allocation_info(device: &Device, desc: &D3D12_RESOURCE_DESC) -> D3D12_RESOURCE_ALLOCATION_INFO {
    let mut info = D3D12_RESOURCE_ALLOCATION_INFO::default();
    let d = &device.device;
    // SAFETY: valid interface; one description; the struct is returned through `info`.
    unsafe { (d.vtbl().GetResourceAllocationInfo)(d.as_raw(), &mut info, 0, 1, desc) };
    info
}

// --- Buffers ---

pub(crate) struct Buffer {
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) address: u64,
    pub(super) mapped: Option<NonNull<u8>>,
    /// Upload and readback buffers keep their initial state on legacy D3D12 (no barriers).
    pub(super) host: bool,
    /// Placed in a heap: its memory may alias other resources'.
    pub(super) placed: bool,
    /// Acceleration structure storage: always in the acceleration structure state.
    pub(super) acceleration_structure: bool,
    _heap: Option<Com<ID3D12Heap>>,
}

// SAFETY: the mapping is process memory owned by the resource; the public API makes access
// through it `unsafe`.
unsafe impl Send for Buffer {}
// SAFETY: as above.
unsafe impl Sync for Buffer {}

fn heap_properties(ty: i32) -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES {
        Type: ty,
        CPUPageProperty: 0,
        MemoryPoolPreference: 0,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    }
}

/// Where a new resource is created: its own committed memory (heap type and flags), a heap at an
/// offset, or no memory at all (sparse).
#[derive(Clone, Copy)]
enum Placement<'a> {
    Committed(i32, u32),
    Placed(&'a Heap, u64),
    Reserved,
}

fn create_resource(
    device: &Com<ID3D12Device>,
    placement: Placement<'_>,
    desc: &D3D12_RESOURCE_DESC,
    state: u32,
    clear: Option<&D3D12_CLEAR_VALUE>,
) -> Result<Com<ID3D12Resource>> {
    let clear = clear.map_or(ptr::null(), |c| c as *const _);
    match placement {
        Placement::Committed(ty, flags) => {
            let heap = heap_properties(ty);
            // SAFETY: valid interface, descriptions, IID and out pointer.
            com::create(|iid, out| unsafe {
                (device.vtbl().CreateCommittedResource)(
                    device.as_raw(),
                    &heap,
                    flags,
                    desc,
                    state,
                    clear,
                    iid,
                    out,
                )
            })
        }
        Placement::Placed(heap, offset) => {
            // SAFETY: valid interfaces, description, IID and out pointer; `api.rs` checked that
            // the offset is aligned and the resource fits.
            com::create(|iid, out| unsafe {
                (device.vtbl().CreatePlacedResource)(
                    device.as_raw(),
                    heap.heap.as_raw(),
                    offset,
                    desc,
                    state,
                    clear,
                    iid,
                    out,
                )
            })
        }
        Placement::Reserved => {
            // SAFETY: valid interface, description, IID and out pointer.
            com::create(|iid, out| unsafe {
                (device.vtbl().CreateReservedResource)(
                    device.as_raw(),
                    desc,
                    state,
                    clear,
                    iid,
                    out,
                )
            })
        }
    }
    .map_err(error)
}

pub(super) fn buffer_resource_desc(size: u64, uav: bool) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        // Rounded so a uniform view of the tail can round its size up too.
        Width: size.next_multiple_of(UNIFORM_ALIGNMENT),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: match uav {
            true => D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            false => 0,
        },
    }
}

fn buffer_desc(desc: &BufferDesc) -> D3D12_RESOURCE_DESC {
    buffer_resource_desc(desc.size, desc.usage.contains(BufferUsage::SHADER_WRITE))
}

pub(super) fn buffer_requirements(device: &Device, desc: &BufferDesc) -> MemoryRequirements {
    let info = allocation_info(device, &buffer_desc(desc));
    MemoryRequirements {
        size: info.SizeInBytes,
        alignment: info.Alignment,
    }
}

/// The state a buffer of `memory` is created in (and, for upload and readback, stays in).
fn initial_buffer_state(memory: MemoryLocation) -> u32 {
    match memory {
        MemoryLocation::GpuOnly | MemoryLocation::GpuUpload => D3D12_RESOURCE_STATE_COMMON,
        MemoryLocation::Upload => D3D12_RESOURCE_STATE_GENERIC_READ,
        MemoryLocation::Readback => D3D12_RESOURCE_STATE_COPY_DEST,
    }
}

/// Maps a CPU-visible buffer for its whole life.
fn map(resource: &Com<ID3D12Resource>, memory: MemoryLocation) -> Result<Option<NonNull<u8>>> {
    if !memory.is_mapped() {
        return Ok(None);
    }
    // The CPU never reads upload memory; readback reads may cover all of it.
    let no_read = D3D12_RANGE { Begin: 0, End: 0 };
    let read = match memory {
        MemoryLocation::Readback => ptr::null(),
        _ => &raw const no_read,
    };
    let mut data = ptr::null_mut();
    // SAFETY: valid interface; subresource 0 of a buffer on a CPU-visible heap.
    check(unsafe { (resource.vtbl().Map)(resource.as_raw(), 0, read, &mut data) })?;
    Ok(NonNull::new(data.cast()))
}

/// A buffer over an existing resource (imported or wrapped).
pub(super) fn wrap_buffer(resource: Com<ID3D12Resource>, desc: &BufferDesc) -> Result<Buffer> {
    // SAFETY: valid interface.
    let address = unsafe { (resource.vtbl().GetGPUVirtualAddress)(resource.as_raw()) };
    Ok(Buffer {
        mapped: map(&resource, desc.memory)?,
        resource,
        address,
        host: matches!(
            desc.memory,
            MemoryLocation::Upload | MemoryLocation::Readback
        ),
        placed: false,
        acceleration_structure: false,
        _heap: None,
    })
}

fn new_buffer(device: &Device, desc: &BufferDesc, placement: Placement<'_>) -> Result<Buffer> {
    let resource = create_resource(
        &device.device,
        placement,
        &buffer_desc(desc),
        initial_buffer_state(desc.memory),
        None,
    )?;
    let mut buffer = wrap_buffer(resource, desc)?;
    if let Placement::Placed(heap, _) = placement {
        buffer.placed = true;
        buffer._heap = Some(heap.heap.clone());
    }
    Ok(buffer)
}

fn committed_flags(shared: bool) -> u32 {
    if shared { D3D12_HEAP_FLAG_SHARED } else { 0 }
}

pub(super) fn create_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    let shared = desc.usage.contains(BufferUsage::SHARED);
    new_buffer(
        device,
        desc,
        Placement::Committed(heap_type(desc.memory), committed_flags(shared)),
    )
}

pub(super) fn create_placed_buffer(
    device: &Device,
    heap: &Heap,
    offset: u64,
    desc: &BufferDesc,
) -> Result<Buffer> {
    new_buffer(device, desc, Placement::Placed(heap, offset))
}

pub(super) fn create_sparse_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    new_buffer(device, desc, Placement::Reserved)
}

/// Committed storage for an acceleration structure: device-local, unordered access. With
/// enhanced barriers it carries the acceleration structure resource flag (barriers then name its
/// accesses); otherwise it is created in, and never leaves, the acceleration structure state.
pub(super) fn create_acceleration_structure_buffer(device: &Device, size: u64) -> Result<Buffer> {
    let resource = match &device.device10 {
        Some(d) => {
            let b = buffer_resource_desc(size, true);
            let desc = D3D12_RESOURCE_DESC1 {
                Dimension: b.Dimension,
                Alignment: b.Alignment,
                Width: b.Width,
                Height: b.Height,
                DepthOrArraySize: b.DepthOrArraySize,
                MipLevels: b.MipLevels,
                Format: b.Format,
                SampleDesc: b.SampleDesc,
                Layout: b.Layout,
                Flags: b.Flags | D3D12_RESOURCE_FLAG_RAYTRACING_ACCELERATION_STRUCTURE,
                SamplerFeedbackMipRegion: [0; 3],
            };
            let heap = heap_properties(D3D12_HEAP_TYPE_DEFAULT);
            // SAFETY: valid interface, descriptions, IID and out pointer; buffers start in the
            // undefined layout; no protected session or castable formats.
            com::create(|iid, out| unsafe {
                (d.vtbl().CreateCommittedResource3)(
                    d.as_raw(),
                    &heap,
                    0,
                    &desc,
                    D3D12_BARRIER_LAYOUT_UNDEFINED,
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                    ptr::null(),
                    iid,
                    out,
                )
            })
            .map_err(error)?
        }
        None => create_resource(
            &device.device,
            Placement::Committed(D3D12_HEAP_TYPE_DEFAULT, 0),
            &buffer_resource_desc(size, true),
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
            None,
        )?,
    };
    let mut buffer = wrap_buffer(
        resource,
        &BufferDesc {
            size,
            usage: BufferUsage::SHADER_WRITE,
            memory: MemoryLocation::GpuOnly,
        },
    )?;
    buffer.acceleration_structure = true;
    Ok(buffer)
}

// --- Textures ---

pub(crate) struct Texture {
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) mip_levels: u32,
    pub(super) array_layers: u32,
    pub(super) sample_count: u32,
    /// The resource's format: typeless for sampled depth, linear for swapchain images.
    pub(super) format: u32,
    /// Format planes: 2 for depth-stencil (depth, then stencil), else 1.
    pub(super) planes: u32,
    /// A render target or depth texture: placed ones are discarded when (re)activated.
    pub(super) target: bool,
    pub(super) placed: bool,
    /// How a sparse texture divides into tiles.
    pub(super) sparse: Option<SparseTextureLayout>,
    _heap: Option<Com<ID3D12Heap>>,
}

impl Texture {
    /// A texture over an existing resource (swapchain images, imported and wrapped textures).
    pub(super) fn wrap(resource: Com<ID3D12Resource>, desc: &TextureDesc, format: u32) -> Self {
        Self {
            resource,
            mip_levels: desc.mip_levels,
            array_layers: desc.array_layers,
            sample_count: desc.sample_count,
            format,
            planes: if desc.format.has_stencil() { 2 } else { 1 },
            target: desc
                .usage
                .intersects(TextureUsage::RENDER_TARGET | TextureUsage::DEPTH_STENCIL),
            placed: false,
            sparse: None,
            _heap: None,
        }
    }
}

/// The resource format for `desc`: depth that is also sampled is typeless, so it can be viewed
/// both as depth and as a float (or stencil) texture.
pub(super) fn resource_format(desc: &TextureDesc) -> u32 {
    let sampled = desc.usage.contains(TextureUsage::SAMPLED);
    match desc.format {
        Format::D16Unorm if sampled => DXGI_FORMAT_R16_TYPELESS,
        Format::D32Float if sampled => DXGI_FORMAT_R32_TYPELESS,
        Format::D32FloatS8 if sampled => DXGI_FORMAT_R32G8X24_TYPELESS,
        f => dxgi_format(f),
    }
}

/// Whether a texture fits in 16 standard 4 KiB tiles, so it may be placed at 4 KiB alignment
/// (D3D12 allows that for small textures that are neither render targets nor depth). Only
/// textures that pass are queried at 4 KiB: asking for an alignment the runtime rejects is a
/// validation error.
fn small_texture(desc: &TextureDesc) -> bool {
    if desc.dimension != TextureDimension::D2
        || desc.sample_count > 1
        || desc
            .usage
            .intersects(TextureUsage::RENDER_TARGET | TextureUsage::DEPTH_STENCIL)
    {
        return false;
    }
    // The 64 KiB standard swizzle tile shapes, quartered: 4 KiB tiles in blocks.
    let (tile_w, tile_h) = match desc.format.block_size() {
        1 => (64, 64),
        2 => (64, 32),
        4 => (32, 32),
        8 => (32, 16),
        _ => (16, 16),
    };
    let block = desc.format.block_extent();
    let (w, h) = (desc.width.div_ceil(block), desc.height.div_ceil(block));
    let tiles = w.div_ceil(tile_w) as u64 * h.div_ceil(tile_h) as u64 * desc.array_layers as u64;
    tiles <= 16
}

/// `D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE`: the layout of sparse textures.
const D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE: i32 = 2;

fn texture_resource_desc(device: &Device, desc: &TextureDesc) -> D3D12_RESOURCE_DESC {
    let usage = desc.usage;
    let mut flags = 0;
    if usage.contains(TextureUsage::RENDER_TARGET) {
        flags |= D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    }
    if usage.contains(TextureUsage::DEPTH_STENCIL) {
        flags |= D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL;
        if !usage.contains(TextureUsage::SAMPLED) {
            flags |= D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE;
        }
    }
    if usage.contains(TextureUsage::STORAGE) {
        flags |= D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    }
    let (dimension, depth_or_layers) = match desc.dimension {
        TextureDimension::D2 => (D3D12_RESOURCE_DIMENSION_TEXTURE2D, desc.array_layers),
        TextureDimension::D3 => (D3D12_RESOURCE_DIMENSION_TEXTURE3D, desc.depth),
    };
    let mut resource_desc = D3D12_RESOURCE_DESC {
        Dimension: dimension,
        Alignment: 0,
        Width: desc.width as u64,
        Height: desc.height,
        DepthOrArraySize: depth_or_layers as u16,
        MipLevels: desc.mip_levels as u16,
        Format: resource_format(desc),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: desc.sample_count,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    };
    if small_texture(desc) {
        resource_desc.Alignment = D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT;
        if allocation_info(device, &resource_desc).Alignment
            != D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT
        {
            resource_desc.Alignment = 0;
        }
    }
    resource_desc
}

pub(super) fn texture_requirements(device: &Device, desc: &TextureDesc) -> MemoryRequirements {
    let info = allocation_info(device, &texture_resource_desc(device, desc));
    MemoryRequirements {
        size: info.SizeInBytes,
        alignment: info.Alignment,
    }
}

/// The format of depth views and clears.
pub(super) fn depth_format(format: Format) -> u32 {
    match format {
        Format::D16Unorm => DXGI_FORMAT_D16_UNORM,
        Format::D32FloatS8 => DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
        _ => DXGI_FORMAT_D32_FLOAT,
    }
}

fn new_texture(device: &Device, desc: &TextureDesc, placement: Placement<'_>) -> Result<Texture> {
    known_format(desc.format)?;
    let mut resource_desc = texture_resource_desc(device, desc);
    match placement {
        // Committed and reserved resources take the default alignment.
        Placement::Committed(..) => resource_desc.Alignment = 0,
        Placement::Reserved => {
            resource_desc.Alignment = 0;
            resource_desc.Layout = D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE;
        }
        Placement::Placed(..) => {}
    }
    let clear = desc.clear_value.map(|value| match value {
        ClearValue::Color(color) => D3D12_CLEAR_VALUE {
            Format: dxgi_format(desc.format),
            Color: color,
        },
        // Stencil (the union's next byte) clears to 0.
        ClearValue::Depth(depth) => D3D12_CLEAR_VALUE {
            Format: depth_format(desc.format),
            Color: [depth, 0.0, 0.0, 0.0],
        },
    });
    // New textures start in the common state (legacy) / common layout (enhanced barriers).
    let resource = create_resource(
        &device.device,
        placement,
        &resource_desc,
        D3D12_RESOURCE_STATE_COMMON,
        clear.as_ref(),
    )?;
    let mut texture = Texture::wrap(resource, desc, resource_desc.Format);
    match placement {
        Placement::Placed(heap, _) => {
            texture.placed = true;
            texture._heap = Some(heap.heap.clone());
        }
        Placement::Reserved => texture.sparse = Some(sparse_layout(device, &texture)),
        Placement::Committed(..) => {}
    }
    Ok(texture)
}

pub(super) fn create_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    let shared = desc.usage.contains(TextureUsage::SHARED);
    new_texture(
        device,
        desc,
        Placement::Committed(D3D12_HEAP_TYPE_DEFAULT, committed_flags(shared)),
    )
}

pub(super) fn create_placed_texture(
    device: &Device,
    heap: &Heap,
    offset: u64,
    desc: &TextureDesc,
) -> Result<Texture> {
    new_texture(device, desc, Placement::Placed(heap, offset))
}

pub(super) fn create_sparse_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    new_texture(device, desc, Placement::Reserved)
}

/// How a reserved texture divides into tiles (`GetResourceTiling`).
fn sparse_layout(device: &Device, texture: &Texture) -> SparseTextureLayout {
    let mut total = 0;
    let mut packed = D3D12_PACKED_MIP_INFO::default();
    let mut shape = D3D12_TILE_SHAPE::default();
    let mut count = 0;
    let d = &device.device;
    // SAFETY: valid interfaces and out pointers; no per-subresource tilings requested.
    unsafe {
        (d.vtbl().GetResourceTiling)(
            d.as_raw(),
            texture.resource.as_raw(),
            &mut total,
            &mut packed,
            &mut shape,
            &mut count,
            0,
            ptr::null_mut(),
        )
    };
    SparseTextureLayout {
        tile_extent: [
            shape.WidthInTexels,
            shape.HeightInTexels,
            shape.DepthInTexels,
        ],
        tile_count: total,
        standard_mips: packed.NumStandardMips as u32,
        packed_mip_tiles: match packed.NumPackedMips {
            0 => 0,
            _ => packed.NumTilesForPackedMips,
        },
    }
}

// --- Views ---

pub(crate) struct RenderTargetView {
    pub(super) handle: D3D12_CPU_DESCRIPTOR_HANDLE,
    /// `None` for swapchain views, which live in the swapchain's own heap.
    pub(super) _slot: Option<Descriptor>,
}

pub(crate) struct DepthStencilView {
    pub(super) slot: Descriptor,
    /// The format has stencil: render passes clear it too.
    pub(super) stencil: bool,
}

pub(super) fn create_render_target_view(
    device: &Device,
    texture: &PublicTexture,
    desc: &RenderTargetViewDesc,
) -> Result<RenderTargetView> {
    let slot = device.bindless.rtv.take()?;
    let format = known_format(desc.format.unwrap_or(texture.desc.format))?;
    let t = &texture.desc;
    let (dimension, u) = match (t.sample_count > 1, t.array_layers > 1) {
        (false, false) => (D3D12_RTV_DIMENSION_TEXTURE2D, [desc.mip, 0, 0, 0]),
        (false, true) => (
            D3D12_RTV_DIMENSION_TEXTURE2DARRAY,
            [desc.mip, desc.layer, 1, 0],
        ),
        (true, false) => (D3D12_RTV_DIMENSION_TEXTURE2DMS, [0; 4]),
        (true, true) => (D3D12_RTV_DIMENSION_TEXTURE2DMSARRAY, [desc.layer, 1, 0, 0]),
    };
    let view = D3D12_RENDER_TARGET_VIEW_DESC {
        Format: format,
        ViewDimension: dimension,
        u: ViewUnion(u),
    };
    let d = &device.device;
    // SAFETY: valid device and resource; the slot is ours.
    unsafe {
        (d.vtbl().CreateRenderTargetView)(
            d.as_raw(),
            texture.raw.resource.as_raw(),
            &view,
            slot.handle(),
        )
    };
    Ok(RenderTargetView {
        handle: slot.handle(),
        _slot: Some(slot),
    })
}

pub(super) fn create_depth_stencil_view(
    device: &Device,
    texture: &PublicTexture,
    desc: &DepthStencilViewDesc,
) -> Result<DepthStencilView> {
    let slot = device.bindless.dsv.take()?;
    let t = &texture.desc;
    let stencil = t.format.has_stencil();
    let flags = match (desc.read_only, stencil) {
        (false, _) => 0,
        (true, false) => D3D12_DSV_FLAG_READ_ONLY_DEPTH,
        (true, true) => D3D12_DSV_FLAG_READ_ONLY_DEPTH | D3D12_DSV_FLAG_READ_ONLY_STENCIL,
    };
    let (dimension, u) = match (t.sample_count > 1, t.array_layers > 1) {
        (false, false) => (D3D12_DSV_DIMENSION_TEXTURE2D, [desc.mip, 0, 0]),
        (false, true) => (
            D3D12_DSV_DIMENSION_TEXTURE2DARRAY,
            [desc.mip, desc.layer, 1],
        ),
        (true, false) => (D3D12_DSV_DIMENSION_TEXTURE2DMS, [0; 3]),
        (true, true) => (D3D12_DSV_DIMENSION_TEXTURE2DMSARRAY, [desc.layer, 1, 0]),
    };
    let view = D3D12_DEPTH_STENCIL_VIEW_DESC {
        Format: depth_format(t.format),
        ViewDimension: dimension,
        Flags: flags,
        u,
    };
    let d = &device.device;
    // SAFETY: valid device and resource; the slot is ours.
    unsafe {
        (d.vtbl().CreateDepthStencilView)(
            d.as_raw(),
            texture.raw.resource.as_raw(),
            &view,
            slot.handle(),
        )
    };
    Ok(DepthStencilView { slot, stencil })
}

/// The view format and plane of `format` on `texture`: depth textures are read through their
/// depth plane (or, as `R8Uint`, their stencil plane).
fn view_format(texture: Format, format: Format) -> Result<(u32, u32)> {
    Ok(match (texture, format) {
        (Format::D32FloatS8, Format::R32Float) => (DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS, 0),
        (Format::D32FloatS8, Format::R8Uint) => (DXGI_FORMAT_X32_TYPELESS_G8X24_UINT, 1),
        (_, f) => (known_format(f)?, 0),
    })
}

/// A writable view of mip `mip`, layers `layers` of `texture` (3D: every slice) in `slot`.
pub(super) fn write_texture_uav(
    device: &Com<ID3D12Device>,
    texture: &PublicTexture,
    format: u32,
    mip: u32,
    layers: (u32, u32),
    handle: D3D12_CPU_DESCRIPTOR_HANDLE,
    array: bool,
) {
    let t = &texture.desc;
    let view = match (t.dimension, array) {
        (TextureDimension::D3, _) => D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE3D,
            // Every depth slice of the mip (WSize -1).
            u: ViewUnion([mip, 0, u32::MAX, 0, 0, 0, 0, 0]),
        },
        (TextureDimension::D2, false) => D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            u: ViewUnion([mip, 0, 0, 0, 0, 0, 0, 0]),
        },
        (TextureDimension::D2, true) => D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2DARRAY,
            u: ViewUnion([mip, layers.0, layers.1, 0, 0, 0, 0, 0]),
        },
    };
    // SAFETY: valid device and resource; no counter; the caller owns the slot.
    unsafe {
        (device.vtbl().CreateUnorderedAccessView)(
            device.as_raw(),
            texture.raw.resource.as_raw(),
            ptr::null_mut(),
            &view,
            handle,
        )
    };
}

/// `desc` is resolved by `api.rs`: concrete format and counts.
pub(super) fn create_texture_view(
    device: &Device,
    texture: &PublicTexture,
    desc: &TextureViewDesc,
) -> Result<Descriptor> {
    let slot = Descriptor::new(&device.bindless.resources)?;
    let t = &texture.desc;
    let (format, plane) = view_format(t.format, desc.format.ok_or_else(invalid)?)?;
    let r = desc.range;
    let d = &device.device;
    let resource = texture.raw.resource.as_raw();
    if desc.writable {
        if desc.dimension == TextureViewDimension::Cube
            || desc.dimension == TextureViewDimension::CubeArray
        {
            return Err(invalid());
        }
        let array = desc.dimension == TextureViewDimension::D2Array;
        write_texture_uav(
            d,
            texture,
            format,
            r.base_mip,
            (r.base_layer, r.layer_count),
            slot.handle(),
            array,
        );
        return Ok(slot);
    }
    let min_lod = 0f32.to_bits();
    let multisampled = t.sample_count > 1;
    let (dimension, u) = match (desc.dimension, multisampled) {
        (TextureViewDimension::D2, false) => (
            D3D12_SRV_DIMENSION_TEXTURE2D,
            [r.base_mip, r.mip_count, plane, min_lod, 0, 0],
        ),
        (TextureViewDimension::D2Array, false) => (
            D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
            [
                r.base_mip,
                r.mip_count,
                r.base_layer,
                r.layer_count,
                plane,
                min_lod,
            ],
        ),
        (TextureViewDimension::D2, true) => (D3D12_SRV_DIMENSION_TEXTURE2DMS, [0; 6]),
        (TextureViewDimension::D2Array, true) => (
            D3D12_SRV_DIMENSION_TEXTURE2DMSARRAY,
            [r.base_layer, r.layer_count, 0, 0, 0, 0],
        ),
        (TextureViewDimension::Cube, _) => (
            D3D12_SRV_DIMENSION_TEXTURECUBE,
            [r.base_mip, r.mip_count, min_lod, 0, 0, 0],
        ),
        (TextureViewDimension::CubeArray, _) => (
            D3D12_SRV_DIMENSION_TEXTURECUBEARRAY,
            [
                r.base_mip,
                r.mip_count,
                r.base_layer,
                r.layer_count / 6,
                min_lod,
                0,
            ],
        ),
        (TextureViewDimension::D3, _) => (
            D3D12_SRV_DIMENSION_TEXTURE3D,
            [r.base_mip, r.mip_count, min_lod, 0, 0, 0],
        ),
    };
    let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: dimension,
        Shader4ComponentMapping: match plane {
            1 => STENCIL_IN_RED_COMPONENT_MAPPING,
            _ => D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        },
        u: ViewUnion(u),
    };
    // SAFETY: valid device and resource; the slot is ours.
    unsafe { (d.vtbl().CreateShaderResourceView)(d.as_raw(), resource, &view, slot.handle()) };
    Ok(slot)
}

/// A raw (`R32_TYPELESS`) view of `size` bytes from `offset`, writable or not, in `handle`.
pub(super) fn write_raw_buffer_view(
    device: &Com<ID3D12Device>,
    buffer: &Buffer,
    offset: u64,
    size: u64,
    writable: bool,
    handle: D3D12_CPU_DESCRIPTOR_HANDLE,
) {
    let first = offset / 4;
    let count = (size / 4) as u32;
    let (first_lo, first_hi) = (first as u32, (first >> 32) as u32);
    let resource = buffer.resource.as_raw();
    if writable {
        let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            u: ViewUnion([
                first_lo,
                first_hi,
                count,
                0,
                0,
                0,
                D3D12_BUFFER_UAV_FLAG_RAW,
                0,
            ]),
        };
        // SAFETY: valid device and resource; no counter; the caller owns the slot.
        unsafe {
            (device.vtbl().CreateUnorderedAccessView)(
                device.as_raw(),
                resource,
                ptr::null_mut(),
                &view,
                handle,
            )
        };
    } else {
        let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            u: ViewUnion([first_lo, first_hi, count, 0, D3D12_BUFFER_SRV_FLAG_RAW, 0]),
        };
        // SAFETY: valid device and resource; the caller owns the slot.
        unsafe {
            (device.vtbl().CreateShaderResourceView)(device.as_raw(), resource, &view, handle)
        };
    }
}

/// `desc` is resolved and validated by `api.rs`: concrete size, aligned offsets.
pub(super) fn create_buffer_view(
    device: &Device,
    buffer: &PublicBuffer,
    desc: &BufferViewDesc,
) -> Result<Descriptor> {
    let slot = Descriptor::new(&device.bindless.resources)?;
    let d = &device.device;
    let b = &buffer.raw;
    let stride = match desc.kind {
        BufferViewKind::Uniform => {
            let view = D3D12_CONSTANT_BUFFER_VIEW_DESC {
                BufferLocation: b.address + desc.offset,
                // The resource is padded to the alignment (`create_buffer`).
                SizeInBytes: desc.size.next_multiple_of(UNIFORM_ALIGNMENT) as u32,
            };
            // SAFETY: valid device; the range is inside the resource; the slot is ours.
            unsafe { (d.vtbl().CreateConstantBufferView)(d.as_raw(), &view, slot.handle()) };
            return Ok(slot);
        }
        BufferViewKind::Raw => {
            write_raw_buffer_view(d, b, desc.offset, desc.size, desc.writable, slot.handle());
            return Ok(slot);
        }
        BufferViewKind::Structured { stride } => stride,
    };
    let first = desc.offset / stride as u64;
    let count = (desc.size / stride as u64) as u32;
    let (first_lo, first_hi) = (first as u32, (first >> 32) as u32);
    let resource = b.resource.as_raw();
    if desc.writable {
        let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_UNKNOWN,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            u: ViewUnion([first_lo, first_hi, count, stride, 0, 0, 0, 0]),
        };
        // SAFETY: valid device and resource; no counter; the slot is ours.
        unsafe {
            (d.vtbl().CreateUnorderedAccessView)(
                d.as_raw(),
                resource,
                ptr::null_mut(),
                &view,
                slot.handle(),
            )
        };
    } else {
        let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_UNKNOWN,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            u: ViewUnion([first_lo, first_hi, count, stride, 0, 0]),
        };
        // SAFETY: valid device and resource; the slot is ours.
        unsafe { (d.vtbl().CreateShaderResourceView)(d.as_raw(), resource, &view, slot.handle()) };
    }
    Ok(slot)
}

// --- Samplers ---

pub(super) fn compare_func(op: CompareOp) -> i32 {
    match op {
        CompareOp::Never => 1,
        CompareOp::Less => 2,
        CompareOp::Equal => 3,
        CompareOp::LessEqual => 4,
        CompareOp::Greater => 5,
        CompareOp::NotEqual => 6,
        CompareOp::GreaterEqual => 7,
        CompareOp::Always => 8,
    }
}

fn address_mode(mode: AddressMode) -> i32 {
    match mode {
        AddressMode::Repeat => 1,
        AddressMode::MirrorRepeat => 2,
        AddressMode::ClampToEdge => 3,
        AddressMode::ClampToBorder => 4,
    }
}

pub(super) fn create_sampler(device: &Device, desc: &SamplerDesc) -> Result<Descriptor> {
    let slot = Descriptor::new(&device.bindless.samplers)?;
    let reduction = match (desc.compare, desc.reduction) {
        (Some(_), _) => D3D12_FILTER_REDUCTION_TYPE_COMPARISON,
        (None, SamplerReduction::WeightedAverage) => 0,
        (None, SamplerReduction::Minimum) => D3D12_FILTER_REDUCTION_TYPE_MINIMUM,
        (None, SamplerReduction::Maximum) => D3D12_FILTER_REDUCTION_TYPE_MAXIMUM,
    };
    let bit = |f: Filter| match f {
        Filter::Nearest => 0,
        Filter::Linear => 1,
    };
    // D3D12_ENCODE_BASIC_FILTER / D3D12_ENCODE_ANISOTROPIC_FILTER.
    let filter = match desc.max_anisotropy {
        1 => {
            (bit(desc.min_filter) << 4)
                | (bit(desc.mag_filter) << 2)
                | bit(desc.mip_filter)
                | (reduction << 7)
        }
        _ => D3D12_FILTER_ANISOTROPIC | (reduction << 7),
    };
    let border = match desc.border_color {
        BorderColor::TransparentBlack => [0.0; 4],
        BorderColor::OpaqueBlack => [0.0, 0.0, 0.0, 1.0],
        BorderColor::OpaqueWhite => [1.0; 4],
    };
    let sampler = D3D12_SAMPLER_DESC {
        Filter: filter,
        AddressU: address_mode(desc.address_u),
        AddressV: address_mode(desc.address_v),
        AddressW: address_mode(desc.address_w),
        MipLODBias: desc.mip_lod_bias,
        MaxAnisotropy: desc.max_anisotropy,
        // Ignored without comparison; NEVER rather than NONE (0), which older runtimes reject.
        ComparisonFunc: desc.compare.map_or(1, compare_func),
        BorderColor: border,
        MinLOD: desc.min_lod,
        MaxLOD: desc.max_lod,
    };
    let d = &device.device;
    // SAFETY: valid device; the slot is ours.
    unsafe { (d.vtbl().CreateSampler)(d.as_raw(), &sampler, slot.handle()) };
    Ok(slot)
}
