//! Buffers, textures, views, samplers and the descriptor heaps behind them.
//!
//! Bindless: buffer and texture views live in one shader-visible CBV/SRV/UAV heap, samplers in
//! one shader-visible sampler heap, and a view's slot is its shader-side index (SM 6.6
//! `ResourceDescriptorHeap[i]` / `SamplerDescriptorHeap[i]`). Render target and depth views live
//! in CPU-only heaps. Every slot is returned to its heap's free list when its owner drops.

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::com::{self, *};
use super::ffi::*;
use super::{Device, check, error};
use crate::{
    AddressMode, BorderColor, Buffer as PublicBuffer, BufferDesc, BufferUsage, BufferViewDesc,
    BufferViewKind, ClearValue, CompareOp, DepthStencilViewDesc, Error, ErrorKind, Filter, Format,
    MemoryLocation, RenderTargetViewDesc, Result, SamplerDesc, Texture as PublicTexture,
    TextureDesc, TextureUsage, TextureViewDesc, TextureViewDimension, UNIFORM_ALIGNMENT,
};

pub(super) fn dxgi_format(format: Format) -> u32 {
    match format {
        Format::R8Unorm => DXGI_FORMAT_R8_UNORM,
        Format::Rg8Unorm => DXGI_FORMAT_R8G8_UNORM,
        Format::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        Format::Rgba8UnormSrgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        Format::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        Format::Bgra8UnormSrgb => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        Format::Rgb10a2Unorm => DXGI_FORMAT_R10G10B10A2_UNORM,
        Format::Rg11b10Float => DXGI_FORMAT_R11G11B10_FLOAT,
        Format::R16Float => DXGI_FORMAT_R16_FLOAT,
        Format::Rg16Float => DXGI_FORMAT_R16G16_FLOAT,
        Format::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        Format::R32Uint => DXGI_FORMAT_R32_UINT,
        Format::R32Float => DXGI_FORMAT_R32_FLOAT,
        Format::Rg32Float => DXGI_FORMAT_R32G32_FLOAT,
        Format::Rgba32Float => DXGI_FORMAT_R32G32B32A32_FLOAT,
        Format::D32Float => DXGI_FORMAT_D32_FLOAT,
        Format::Bc1Unorm => DXGI_FORMAT_BC1_UNORM,
        Format::Bc1UnormSrgb => DXGI_FORMAT_BC1_UNORM_SRGB,
        Format::Bc3Unorm => DXGI_FORMAT_BC3_UNORM,
        Format::Bc3UnormSrgb => DXGI_FORMAT_BC3_UNORM_SRGB,
        Format::Bc4Unorm => DXGI_FORMAT_BC4_UNORM,
        Format::Bc5Unorm => DXGI_FORMAT_BC5_UNORM,
        Format::Bc6hUfloat => DXGI_FORMAT_BC6H_UF16,
        Format::Bc7Unorm => DXGI_FORMAT_BC7_UNORM,
        Format::Bc7UnormSrgb => DXGI_FORMAT_BC7_UNORM_SRGB,
    }
}

fn invalid() -> Error {
    Error::from_kind(ErrorKind::InvalidInput)
}

// --- Descriptor heaps ---

pub(super) struct DescriptorHeap {
    heap: Com<ID3D12DescriptorHeap>,
    start: usize,
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
        // SAFETY: valid interface.
        let increment =
            unsafe { (device.vtbl().GetDescriptorHandleIncrementSize)(device.as_raw(), ty) };
        Ok(Arc::new(Self {
            heap,
            start: start.ptr,
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
}

/// One slot of a descriptor heap, returned to it on drop.
pub(crate) struct Descriptor {
    heap: Arc<DescriptorHeap>,
    index: u32,
}

impl Descriptor {
    pub(super) fn new(heap: &Arc<DescriptorHeap>) -> Result<Self> {
        let mut list = heap.free.lock();
        let index = match list.freed.pop() {
            Some(index) => index,
            None if list.next < heap.capacity => {
                list.next += 1;
                list.next - 1
            }
            None => return Err(Error::from_kind(ErrorKind::OutOfMemory)),
        };
        Ok(Self {
            heap: heap.clone(),
            index,
        })
    }

    pub(super) fn handle(&self) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.heap.start + self.index as usize * self.heap.increment,
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

// --- Buffers ---

pub(crate) struct Buffer {
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) address: u64,
    pub(super) mapped: Option<NonNull<u8>>,
    /// Upload and readback buffers keep their initial state on legacy D3D12 (no barriers).
    pub(super) host: bool,
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

fn create_committed(
    device: &Com<ID3D12Device>,
    heap_type: i32,
    desc: &D3D12_RESOURCE_DESC,
    state: u32,
    clear: Option<&D3D12_CLEAR_VALUE>,
) -> Result<Com<ID3D12Resource>> {
    let heap = heap_properties(heap_type);
    let clear = clear.map_or(ptr::null(), |c| c as *const _);
    // SAFETY: valid interface, descriptions, IID and out pointer.
    com::create(|iid, out| unsafe {
        (device.vtbl().CreateCommittedResource)(
            device.as_raw(),
            &heap,
            0,
            desc,
            state,
            clear,
            iid,
            out,
        )
    })
    .map_err(error)
}

pub(super) fn create_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    let (heap_type, state) = match desc.memory {
        MemoryLocation::GpuOnly => (D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COMMON),
        MemoryLocation::Upload => (D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ),
        MemoryLocation::Readback => (D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST),
    };
    let flags = match desc.usage.contains(BufferUsage::SHADER_WRITE) {
        true => D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        false => 0,
    };
    let resource_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        // Rounded so a uniform view of the tail can round its size up too.
        Width: desc.size.next_multiple_of(UNIFORM_ALIGNMENT),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
    };
    let resource = create_committed(&device.device, heap_type, &resource_desc, state, None)?;
    // SAFETY: valid interface.
    let address = unsafe { (resource.vtbl().GetGPUVirtualAddress)(resource.as_raw()) };
    let host = desc.memory != MemoryLocation::GpuOnly;
    let mapped = if host {
        // The CPU never reads upload memory; readback reads may cover all of it.
        let no_read = D3D12_RANGE { Begin: 0, End: 0 };
        let read = match desc.memory {
            MemoryLocation::Upload => &raw const no_read,
            _ => ptr::null(),
        };
        let mut data = ptr::null_mut();
        // SAFETY: valid interface; subresource 0 of a buffer on a CPU-visible heap.
        check(unsafe { (resource.vtbl().Map)(resource.as_raw(), 0, read, &mut data) })?;
        NonNull::new(data.cast())
    } else {
        None
    };
    Ok(Buffer {
        resource,
        address,
        mapped,
        host,
    })
}

// --- Textures ---

pub(crate) struct Texture {
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) mip_levels: u32,
    pub(super) array_layers: u32,
    /// The resource's format: typeless for sampled depth, linear for swapchain images.
    pub(super) format: u32,
}

/// The resource format for `desc`: depth that is also sampled is typeless, so it can be viewed
/// as both `D32_FLOAT` and `R32_FLOAT`.
fn resource_format(desc: &TextureDesc) -> u32 {
    match desc.format {
        Format::D32Float if desc.usage.contains(TextureUsage::SAMPLED) => DXGI_FORMAT_R32_TYPELESS,
        f => dxgi_format(f),
    }
}

pub(super) fn create_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
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
    let format = resource_format(desc);
    let resource_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: desc.width as u64,
        Height: desc.height,
        DepthOrArraySize: desc.array_layers as u16,
        MipLevels: desc.mip_levels as u16,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    };
    let clear = desc.clear_value.map(|value| match value {
        ClearValue::Color(color) => D3D12_CLEAR_VALUE {
            Format: dxgi_format(desc.format),
            Color: color,
        },
        ClearValue::Depth(depth) => D3D12_CLEAR_VALUE {
            Format: DXGI_FORMAT_D32_FLOAT,
            Color: [depth, 0.0, 0.0, 0.0],
        },
    });
    // New textures start in the common state (legacy) / common layout (enhanced barriers).
    let resource = create_committed(
        &device.device,
        D3D12_HEAP_TYPE_DEFAULT,
        &resource_desc,
        D3D12_RESOURCE_STATE_COMMON,
        clear.as_ref(),
    )?;
    Ok(Texture {
        resource,
        mip_levels: desc.mip_levels,
        array_layers: desc.array_layers,
        format,
    })
}

// --- Views ---

pub(crate) struct RenderTargetView {
    pub(super) handle: D3D12_CPU_DESCRIPTOR_HANDLE,
    /// `None` for swapchain views, which live in the swapchain's own heap.
    pub(super) _slot: Option<Descriptor>,
}

pub(crate) struct DepthStencilView {
    pub(super) slot: Descriptor,
}

pub(super) fn create_render_target_view(
    device: &Device,
    texture: &PublicTexture,
    desc: &RenderTargetViewDesc,
) -> Result<RenderTargetView> {
    let slot = Descriptor::new(&device.bindless.rtv)?;
    let format = dxgi_format(desc.format.unwrap_or(texture.desc.format));
    let view = match texture.desc.array_layers {
        1 => D3D12_RENDER_TARGET_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
            u: ViewUnion([desc.mip, 0, 0, 0]),
        },
        _ => D3D12_RENDER_TARGET_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2DARRAY,
            u: ViewUnion([desc.mip, desc.layer, 1, 0]),
        },
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
    let slot = Descriptor::new(&device.bindless.dsv)?;
    let flags = match desc.read_only {
        true => D3D12_DSV_FLAG_READ_ONLY_DEPTH,
        false => 0,
    };
    let view = match texture.desc.array_layers {
        1 => D3D12_DEPTH_STENCIL_VIEW_DESC {
            Format: DXGI_FORMAT_D32_FLOAT,
            ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
            Flags: flags,
            u: [desc.mip, 0, 0],
        },
        _ => D3D12_DEPTH_STENCIL_VIEW_DESC {
            Format: DXGI_FORMAT_D32_FLOAT,
            ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2DARRAY,
            Flags: flags,
            u: [desc.mip, desc.layer, 1],
        },
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
    Ok(DepthStencilView { slot })
}

/// `desc` is resolved by `api.rs`: concrete format and counts.
pub(super) fn create_texture_view(
    device: &Device,
    texture: &PublicTexture,
    desc: &TextureViewDesc,
) -> Result<Descriptor> {
    let slot = Descriptor::new(&device.bindless.resources)?;
    let format = dxgi_format(desc.format.ok_or_else(invalid)?);
    let r = desc.range;
    let d = &device.device;
    let resource = texture.raw.resource.as_raw();
    if desc.writable {
        let view = match desc.dimension {
            TextureViewDimension::D2 => D3D12_UNORDERED_ACCESS_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
                u: ViewUnion([r.base_mip, 0, 0, 0, 0, 0, 0, 0]),
            },
            TextureViewDimension::D2Array => D3D12_UNORDERED_ACCESS_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2DARRAY,
                u: ViewUnion([r.base_mip, r.base_layer, r.layer_count, 0, 0, 0, 0, 0]),
            },
            TextureViewDimension::Cube => return Err(invalid()),
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
        let min_lod = 0f32.to_bits();
        let (dimension, u) = match desc.dimension {
            TextureViewDimension::D2 => (
                D3D12_SRV_DIMENSION_TEXTURE2D,
                [r.base_mip, r.mip_count, 0, min_lod, 0, 0],
            ),
            TextureViewDimension::D2Array => (
                D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
                [
                    r.base_mip,
                    r.mip_count,
                    r.base_layer,
                    r.layer_count,
                    0,
                    min_lod,
                ],
            ),
            TextureViewDimension::Cube => (
                D3D12_SRV_DIMENSION_TEXTURECUBE,
                [r.base_mip, r.mip_count, min_lod, 0, 0, 0],
            ),
        };
        let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: dimension,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            u: ViewUnion(u),
        };
        // SAFETY: valid device and resource; the slot is ours.
        unsafe { (d.vtbl().CreateShaderResourceView)(d.as_raw(), resource, &view, slot.handle()) };
    }
    Ok(slot)
}

/// `desc` is resolved and validated by `api.rs`: concrete size, aligned offsets.
pub(super) fn create_buffer_view(
    device: &Device,
    buffer: &PublicBuffer,
    desc: &BufferViewDesc,
) -> Result<Descriptor> {
    let slot = Descriptor::new(&device.bindless.resources)?;
    let d = &device.device;
    let resource = buffer.raw.resource.as_raw();
    let (format, element_size, stride, raw_flag) = match desc.kind {
        BufferViewKind::Uniform => {
            let view = D3D12_CONSTANT_BUFFER_VIEW_DESC {
                BufferLocation: buffer.raw.address + desc.offset,
                // The resource is padded to the alignment (`create_buffer`).
                SizeInBytes: desc.size.next_multiple_of(UNIFORM_ALIGNMENT) as u32,
            };
            // SAFETY: valid device; the range is inside the resource; the slot is ours.
            unsafe { (d.vtbl().CreateConstantBufferView)(d.as_raw(), &view, slot.handle()) };
            return Ok(slot);
        }
        BufferViewKind::Structured { stride } => (DXGI_FORMAT_UNKNOWN, stride, stride, 0),
        BufferViewKind::Raw => (DXGI_FORMAT_R32_TYPELESS, 4, 0, 1),
    };
    let first = desc.offset / element_size as u64;
    let count = (desc.size / element_size as u64) as u32;
    let (first_lo, first_hi) = (first as u32, (first >> 32) as u32);
    if desc.writable {
        let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            u: ViewUnion([
                first_lo,
                first_hi,
                count,
                stride,
                0,
                0,
                raw_flag * D3D12_BUFFER_UAV_FLAG_RAW,
                0,
            ]),
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
            Format: format,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            u: ViewUnion([
                first_lo,
                first_hi,
                count,
                stride,
                raw_flag * D3D12_BUFFER_SRV_FLAG_RAW,
                0,
            ]),
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
    let reduction = match desc.compare {
        Some(_) => D3D12_FILTER_REDUCTION_TYPE_COMPARISON,
        None => 0,
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
