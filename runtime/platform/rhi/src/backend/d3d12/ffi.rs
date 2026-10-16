//! D3D12 / DXGI structures and constants (from the Windows SDK 10.0.26100 headers), and the
//! three DLL entry points, loaded at run time.

#![allow(non_camel_case_types, non_snake_case)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::LUID;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::core::{BOOL, GUID, HRESULT, PCSTR};
use windows_sys::{s, w};

// --- HRESULTs ---

pub(super) const E_POINTER: HRESULT = 0x8000_4003_u32 as i32;
pub(super) const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
pub(super) const E_NOTIMPL: HRESULT = 0x8000_4001_u32 as i32;
pub(super) const E_OUTOFMEMORY: HRESULT = 0x8007_000E_u32 as i32;
pub(super) const E_INVALIDARG: HRESULT = 0x8007_0057_u32 as i32;
pub(super) const DXGI_STATUS_OCCLUDED: HRESULT = 0x087A_0001;
pub(super) const DXGI_ERROR_INVALID_CALL: HRESULT = 0x887A_0001_u32 as i32;
pub(super) const DXGI_ERROR_NOT_FOUND: HRESULT = 0x887A_0002_u32 as i32;
pub(super) const DXGI_ERROR_UNSUPPORTED: HRESULT = 0x887A_0004_u32 as i32;
pub(super) const DXGI_ERROR_DEVICE_REMOVED: HRESULT = 0x887A_0005_u32 as i32;
pub(super) const DXGI_ERROR_DEVICE_HUNG: HRESULT = 0x887A_0006_u32 as i32;
pub(super) const DXGI_ERROR_DEVICE_RESET: HRESULT = 0x887A_0007_u32 as i32;
pub(super) const DXGI_ERROR_DRIVER_INTERNAL_ERROR: HRESULT = 0x887A_0020_u32 as i32;

// --- DXGI ---

pub(super) const DXGI_CREATE_FACTORY_DEBUG: u32 = 0x1;
pub(super) const DXGI_MWA_NO_ALT_ENTER: u32 = 1 << 1;
pub(super) const DXGI_FEATURE_PRESENT_ALLOW_TEARING: i32 = 0;
pub(super) const DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE: i32 = 2;
pub(super) const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;
pub(super) const DXGI_USAGE_RENDER_TARGET_OUTPUT: u32 = 0x20;
pub(super) const DXGI_SCALING_STRETCH: i32 = 0;
pub(super) const DXGI_SWAP_EFFECT_FLIP_DISCARD: i32 = 4;
pub(super) const DXGI_ALPHA_MODE_IGNORE: i32 = 3;
pub(super) const DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT: u32 = 64;
pub(super) const DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING: u32 = 2048;
pub(super) const DXGI_PRESENT_ALLOW_TEARING: u32 = 0x200;

pub(super) const DXGI_FORMAT_UNKNOWN: u32 = 0;
pub(super) const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
pub(super) const DXGI_FORMAT_R16G16B16A16_FLOAT: u32 = 10;
pub(super) const DXGI_FORMAT_R32G32_FLOAT: u32 = 16;
pub(super) const DXGI_FORMAT_R10G10B10A2_UNORM: u32 = 24;
pub(super) const DXGI_FORMAT_R11G11B10_FLOAT: u32 = 26;
pub(super) const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
pub(super) const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = 29;
pub(super) const DXGI_FORMAT_R16G16_FLOAT: u32 = 34;
pub(super) const DXGI_FORMAT_R32_TYPELESS: u32 = 39;
pub(super) const DXGI_FORMAT_D32_FLOAT: u32 = 40;
pub(super) const DXGI_FORMAT_R32_FLOAT: u32 = 41;
pub(super) const DXGI_FORMAT_R32_UINT: u32 = 42;
pub(super) const DXGI_FORMAT_R8G8_UNORM: u32 = 49;
pub(super) const DXGI_FORMAT_R16_FLOAT: u32 = 54;
pub(super) const DXGI_FORMAT_R16_UINT: u32 = 57;
pub(super) const DXGI_FORMAT_R8_UNORM: u32 = 61;
pub(super) const DXGI_FORMAT_BC1_UNORM: u32 = 71;
pub(super) const DXGI_FORMAT_BC1_UNORM_SRGB: u32 = 72;
pub(super) const DXGI_FORMAT_BC3_UNORM: u32 = 77;
pub(super) const DXGI_FORMAT_BC3_UNORM_SRGB: u32 = 78;
pub(super) const DXGI_FORMAT_BC4_UNORM: u32 = 80;
pub(super) const DXGI_FORMAT_BC5_UNORM: u32 = 83;
pub(super) const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
pub(super) const DXGI_FORMAT_B8G8R8A8_UNORM_SRGB: u32 = 91;
pub(super) const DXGI_FORMAT_BC6H_UF16: u32 = 95;
pub(super) const DXGI_FORMAT_BC7_UNORM: u32 = 98;
pub(super) const DXGI_FORMAT_BC7_UNORM_SRGB: u32 = 99;

#[repr(C)]
pub(super) struct DXGI_ADAPTER_DESC1 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: usize,
    pub DedicatedSystemMemory: usize,
    pub SharedSystemMemory: usize,
    pub AdapterLuid: LUID,
    pub Flags: u32,
}

#[repr(C)]
pub(super) struct DXGI_SAMPLE_DESC {
    pub Count: u32,
    pub Quality: u32,
}

#[repr(C)]
pub(super) struct DXGI_SWAP_CHAIN_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub Format: u32,
    pub Stereo: BOOL,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub BufferUsage: u32,
    pub BufferCount: u32,
    pub Scaling: i32,
    pub SwapEffect: i32,
    pub AlphaMode: i32,
    pub Flags: u32,
}

// --- D3D12 ---

pub(super) const D3D_FEATURE_LEVEL_12_0: i32 = 0xc000;
pub(super) const D3D_SHADER_MODEL_6_6: i32 = 0x66;
/// The newest shader model the feature query is asked about; older runtimes reject unknown
/// values, so the query steps down from here.
pub(super) const D3D_SHADER_MODEL_HIGHEST: i32 = 0x69;
pub(super) const D3D_ROOT_SIGNATURE_VERSION_1_1: i32 = 2;
pub(super) const D3D12_COMMAND_LIST_TYPE_DIRECT: i32 = 0;
pub(super) const D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV: i32 = 0;
pub(super) const D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER: i32 = 1;
pub(super) const D3D12_DESCRIPTOR_HEAP_TYPE_RTV: i32 = 2;
pub(super) const D3D12_DESCRIPTOR_HEAP_TYPE_DSV: i32 = 3;
pub(super) const D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE: i32 = 1;
pub(super) const D3D12_RTV_DIMENSION_TEXTURE2D: i32 = 4;
pub(super) const D3D12_RTV_DIMENSION_TEXTURE2DARRAY: i32 = 5;
pub(super) const D3D12_DSV_DIMENSION_TEXTURE2D: i32 = 3;
pub(super) const D3D12_DSV_DIMENSION_TEXTURE2DARRAY: i32 = 4;
pub(super) const D3D12_DSV_FLAG_READ_ONLY_DEPTH: u32 = 1;
pub(super) const D3D12_SRV_DIMENSION_BUFFER: i32 = 1;
pub(super) const D3D12_SRV_DIMENSION_TEXTURE2D: i32 = 4;
pub(super) const D3D12_SRV_DIMENSION_TEXTURE2DARRAY: i32 = 5;
pub(super) const D3D12_SRV_DIMENSION_TEXTURECUBE: i32 = 9;
pub(super) const D3D12_UAV_DIMENSION_BUFFER: i32 = 1;
pub(super) const D3D12_UAV_DIMENSION_TEXTURE2D: i32 = 4;
pub(super) const D3D12_UAV_DIMENSION_TEXTURE2DARRAY: i32 = 5;
pub(super) const D3D12_BUFFER_SRV_FLAG_RAW: u32 = 1;
pub(super) const D3D12_BUFFER_UAV_FLAG_RAW: u32 = 1;
pub(super) const D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING: u32 = 0x1688;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS: i32 = 0;
pub(super) const D3D12_FEATURE_SHADER_MODEL: i32 = 7;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS12: i32 = 41;
pub(super) const D3D12_RESOURCE_BINDING_TIER_3: i32 = 3;

pub(super) const D3D12_HEAP_TYPE_DEFAULT: i32 = 1;
pub(super) const D3D12_HEAP_TYPE_UPLOAD: i32 = 2;
pub(super) const D3D12_HEAP_TYPE_READBACK: i32 = 3;
pub(super) const D3D12_RESOURCE_DIMENSION_BUFFER: i32 = 1;
pub(super) const D3D12_RESOURCE_DIMENSION_TEXTURE2D: i32 = 3;
pub(super) const D3D12_TEXTURE_LAYOUT_UNKNOWN: i32 = 0;
pub(super) const D3D12_TEXTURE_LAYOUT_ROW_MAJOR: i32 = 1;
pub(super) const D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET: u32 = 0x1;
pub(super) const D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL: u32 = 0x2;
pub(super) const D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS: u32 = 0x4;
pub(super) const D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE: u32 = 0x8;
pub(super) const D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX: i32 = 0;
pub(super) const D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT: i32 = 1;
pub(super) const D3D12_CLEAR_FLAG_DEPTH: u32 = 1;

pub(super) const D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS: i32 = 1;
pub(super) const D3D12_SHADER_VISIBILITY_ALL: i32 = 0;
pub(super) const D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED: u32 = 0x400;
pub(super) const D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED: u32 = 0x800;

pub(super) const D3D12_FILL_MODE_WIREFRAME: i32 = 2;
pub(super) const D3D12_FILL_MODE_SOLID: i32 = 3;
pub(super) const D3D12_CULL_MODE_NONE: i32 = 1;
pub(super) const D3D12_CULL_MODE_FRONT: i32 = 2;
pub(super) const D3D12_CULL_MODE_BACK: i32 = 3;
pub(super) const D3D12_LOGIC_OP_NOOP: i32 = 4;
pub(super) const D3D12_STENCIL_OP_KEEP: i32 = 1;
pub(super) const D3D12_COMPARISON_FUNC_ALWAYS: i32 = 8;
pub(super) const D3D12_PRIMITIVE_TOPOLOGY_TYPE_POINT: i32 = 1;
pub(super) const D3D12_PRIMITIVE_TOPOLOGY_TYPE_LINE: i32 = 2;
pub(super) const D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE: i32 = 3;
pub(super) const D3D_PRIMITIVE_TOPOLOGY_POINTLIST: i32 = 1;
pub(super) const D3D_PRIMITIVE_TOPOLOGY_LINELIST: i32 = 2;
pub(super) const D3D_PRIMITIVE_TOPOLOGY_LINESTRIP: i32 = 3;
pub(super) const D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST: i32 = 4;
pub(super) const D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP: i32 = 5;

pub(super) const D3D12_FILTER_REDUCTION_TYPE_COMPARISON: i32 = 1;
pub(super) const D3D12_FILTER_ANISOTROPIC: i32 = 0x55;
pub(super) const D3D12_MESSAGE_SEVERITY_CORRUPTION: i32 = 0;
pub(super) const D3D12_MESSAGE_SEVERITY_ERROR: i32 = 1;
pub(super) const D3D12_MESSAGE_SEVERITY_WARNING: i32 = 2;

pub(super) const D3D12_RESOURCE_BARRIER_TYPE_TRANSITION: i32 = 0;
pub(super) const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 = 0xffff_ffff;
pub(super) const D3D12_RESOURCE_STATE_COMMON: u32 = 0;
pub(super) const D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER: u32 = 0x1;
pub(super) const D3D12_RESOURCE_STATE_INDEX_BUFFER: u32 = 0x2;
pub(super) const D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT: u32 = 0x200;
pub(super) const D3D12_RESOURCE_STATE_GENERIC_READ: u32 = 0xac3;
pub(super) const D3D12_RESOURCE_STATE_RENDER_TARGET: u32 = 0x4;
pub(super) const D3D12_RESOURCE_STATE_UNORDERED_ACCESS: u32 = 0x8;
pub(super) const D3D12_RESOURCE_STATE_DEPTH_WRITE: u32 = 0x10;
pub(super) const D3D12_RESOURCE_STATE_DEPTH_READ: u32 = 0x20;
pub(super) const D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE: u32 = 0x40;
pub(super) const D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE: u32 = 0x80;
pub(super) const D3D12_RESOURCE_STATE_COPY_DEST: u32 = 0x400;
pub(super) const D3D12_RESOURCE_STATE_COPY_SOURCE: u32 = 0x800;
pub(super) const D3D12_RESOURCE_STATE_RESOLVE_DEST: u32 = 0x1000;
pub(super) const D3D12_RESOURCE_STATE_RESOLVE_SOURCE: u32 = 0x2000;

pub(super) const D3D12_BARRIER_TYPE_TEXTURE: i32 = 1;
pub(super) const D3D12_BARRIER_TYPE_BUFFER: i32 = 2;
pub(super) const D3D12_BARRIER_ACCESS_NO_ACCESS: u32 = 0x8000_0000;
pub(super) const D3D12_BARRIER_LAYOUT_UNDEFINED: u32 = 0xffff_ffff;
pub(super) const D3D12_BARRIER_LAYOUT_COMMON: u32 = 0;
pub(super) const D3D12_BARRIER_LAYOUT_RENDER_TARGET: u32 = 2;
pub(super) const D3D12_BARRIER_LAYOUT_UNORDERED_ACCESS: u32 = 3;
pub(super) const D3D12_BARRIER_LAYOUT_DEPTH_STENCIL_WRITE: u32 = 4;
pub(super) const D3D12_BARRIER_LAYOUT_DEPTH_STENCIL_READ: u32 = 5;
pub(super) const D3D12_BARRIER_LAYOUT_SHADER_RESOURCE: u32 = 6;
pub(super) const D3D12_BARRIER_LAYOUT_COPY_SOURCE: u32 = 7;
pub(super) const D3D12_BARRIER_LAYOUT_COPY_DEST: u32 = 8;
pub(super) const D3D12_BARRIER_LAYOUT_RESOLVE_SOURCE: u32 = 9;
pub(super) const D3D12_BARRIER_LAYOUT_RESOLVE_DEST: u32 = 10;

#[repr(C)]
pub(super) struct D3D12_COMMAND_QUEUE_DESC {
    pub Type: i32,
    pub Priority: i32,
    pub Flags: i32,
    pub NodeMask: u32,
}

#[repr(C)]
pub(super) struct D3D12_DESCRIPTOR_HEAP_DESC {
    pub Type: i32,
    pub NumDescriptors: u32,
    pub Flags: i32,
    pub NodeMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_CPU_DESCRIPTOR_HANDLE {
    pub ptr: usize,
}

/// The anonymous unions of the view descriptions, as words: `N` 32-bit fields, 8-aligned like the
/// unions (each has a `UINT64` member). Fields are written in declaration order; a `UINT64` takes
/// two words, low first.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(super) struct ViewUnion<const N: usize>(pub [u32; N]);

impl<const N: usize> Default for ViewUnion<N> {
    fn default() -> Self {
        Self([0; N])
    }
}

/// Union: `Texture2D` {MipSlice, PlaneSlice}, `Texture2DArray` {MipSlice, FirstArraySlice,
/// ArraySize, PlaneSlice}, ... (16 bytes).
#[repr(C)]
pub(super) struct D3D12_RENDER_TARGET_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: i32,
    pub u: ViewUnion<4>,
}

/// Union: `Texture2D` {MipSlice}, `Texture2DArray` {MipSlice, FirstArraySlice, ArraySize}, ...
/// (12 bytes, 4-aligned).
#[repr(C)]
pub(super) struct D3D12_DEPTH_STENCIL_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: i32,
    pub Flags: u32,
    pub u: [u32; 3],
}

/// Union: `Buffer` {FirstElement: u64, NumElements, StructureByteStride, Flags},
/// `Texture2D` {MostDetailedMip, MipLevels, PlaneSlice, ResourceMinLODClamp},
/// `Texture2DArray` {MostDetailedMip, MipLevels, FirstArraySlice, ArraySize, PlaneSlice,
/// ResourceMinLODClamp}, `TextureCube` {MostDetailedMip, MipLevels, ResourceMinLODClamp}
/// (24 bytes).
#[repr(C)]
pub(super) struct D3D12_SHADER_RESOURCE_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: i32,
    pub Shader4ComponentMapping: u32,
    pub u: ViewUnion<6>,
}

/// Union: `Buffer` {FirstElement: u64, NumElements, StructureByteStride,
/// CounterOffsetInBytes: u64, Flags}, `Texture2D` {MipSlice, PlaneSlice}, `Texture2DArray`
/// {MipSlice, FirstArraySlice, ArraySize, PlaneSlice} (32 bytes).
#[repr(C)]
pub(super) struct D3D12_UNORDERED_ACCESS_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: i32,
    pub u: ViewUnion<8>,
}

#[repr(C)]
pub(super) struct D3D12_CONSTANT_BUFFER_VIEW_DESC {
    pub BufferLocation: u64,
    pub SizeInBytes: u32,
}

#[repr(C)]
pub(super) struct D3D12_SAMPLER_DESC {
    pub Filter: i32,
    pub AddressU: i32,
    pub AddressV: i32,
    pub AddressW: i32,
    pub MipLODBias: f32,
    pub MaxAnisotropy: u32,
    pub ComparisonFunc: i32,
    pub BorderColor: [f32; 4],
    pub MinLOD: f32,
    pub MaxLOD: f32,
}

#[repr(C)]
pub(super) struct D3D12_HEAP_PROPERTIES {
    pub Type: i32,
    pub CPUPageProperty: i32,
    pub MemoryPoolPreference: i32,
    pub CreationNodeMask: u32,
    pub VisibleNodeMask: u32,
}

#[repr(C)]
pub(super) struct D3D12_RESOURCE_DESC {
    pub Dimension: i32,
    pub Alignment: u64,
    pub Width: u64,
    pub Height: u32,
    pub DepthOrArraySize: u16,
    pub MipLevels: u16,
    pub Format: u32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub Layout: i32,
    pub Flags: u32,
}

/// The union holds `Color: [f32; 4]` or `DepthStencil` {Depth: f32, Stencil: u8}; depth goes in
/// the first float.
#[repr(C)]
pub(super) struct D3D12_CLEAR_VALUE {
    pub Format: u32,
    pub Color: [f32; 4],
}

#[repr(C)]
pub(super) struct D3D12_RANGE {
    pub Begin: usize,
    pub End: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_SUBRESOURCE_FOOTPRINT {
    pub Format: u32,
    pub Width: u32,
    pub Height: u32,
    pub Depth: u32,
    pub RowPitch: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
    pub Offset: u64,
    pub Footprint: D3D12_SUBRESOURCE_FOOTPRINT,
}

/// The union holds `PlacedFootprint` or `SubresourceIndex` (the footprint's first word, as the
/// low half of `Offset`).
#[repr(C)]
pub(super) struct D3D12_TEXTURE_COPY_LOCATION {
    pub pResource: *mut c_void,
    pub Type: i32,
    pub PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
}

#[repr(C)]
pub(super) struct D3D12_BOX {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}

#[repr(C)]
pub(super) struct D3D12_VIEWPORT {
    pub TopLeftX: f32,
    pub TopLeftY: f32,
    pub Width: f32,
    pub Height: f32,
    pub MinDepth: f32,
    pub MaxDepth: f32,
}

#[repr(C)]
pub(super) struct D3D12_RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub(super) struct D3D12_INDEX_BUFFER_VIEW {
    pub BufferLocation: u64,
    pub SizeInBytes: u32,
    pub Format: u32,
}

// --- Root signatures and pipelines ---

/// The `Constants` member of the parameter union (the union is 16 bytes, 8-aligned: its
/// descriptor-table member holds a pointer).
#[repr(C, align(8))]
pub(super) struct D3D12_ROOT_CONSTANTS {
    pub ShaderRegister: u32,
    pub RegisterSpace: u32,
    pub Num32BitValues: u32,
    pub _union: u32,
}

#[repr(C)]
pub(super) struct D3D12_ROOT_PARAMETER1 {
    pub ParameterType: i32,
    pub Constants: D3D12_ROOT_CONSTANTS,
    pub ShaderVisibility: i32,
}

#[repr(C)]
pub(super) struct D3D12_ROOT_SIGNATURE_DESC1 {
    pub NumParameters: u32,
    pub pParameters: *const D3D12_ROOT_PARAMETER1,
    pub NumStaticSamplers: u32,
    pub pStaticSamplers: *const c_void,
    pub Flags: u32,
}

/// The union's other members (versions 1.0 and 1.2) have the same layout.
#[repr(C)]
pub(super) struct D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
    pub Version: i32,
    pub Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_SHADER_BYTECODE {
    pub pShaderBytecode: *const c_void,
    pub BytecodeLength: usize,
}

impl D3D12_SHADER_BYTECODE {
    pub const NONE: Self = Self {
        pShaderBytecode: core::ptr::null(),
        BytecodeLength: 0,
    };
}

#[repr(C)]
pub(super) struct D3D12_STREAM_OUTPUT_DESC {
    pub pSODeclaration: *const c_void,
    pub NumEntries: u32,
    pub pBufferStrides: *const u32,
    pub NumStrides: u32,
    pub RasterizedStream: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_RENDER_TARGET_BLEND_DESC {
    pub BlendEnable: BOOL,
    pub LogicOpEnable: BOOL,
    pub SrcBlend: i32,
    pub DestBlend: i32,
    pub BlendOp: i32,
    pub SrcBlendAlpha: i32,
    pub DestBlendAlpha: i32,
    pub BlendOpAlpha: i32,
    pub LogicOp: i32,
    pub RenderTargetWriteMask: u8,
}

#[repr(C)]
pub(super) struct D3D12_BLEND_DESC {
    pub AlphaToCoverageEnable: BOOL,
    pub IndependentBlendEnable: BOOL,
    pub RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC; 8],
}

#[repr(C)]
pub(super) struct D3D12_RASTERIZER_DESC {
    pub FillMode: i32,
    pub CullMode: i32,
    pub FrontCounterClockwise: BOOL,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: BOOL,
    pub MultisampleEnable: BOOL,
    pub AntialiasedLineEnable: BOOL,
    pub ForcedSampleCount: u32,
    pub ConservativeRaster: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_DEPTH_STENCILOP_DESC {
    pub StencilFailOp: i32,
    pub StencilDepthFailOp: i32,
    pub StencilPassOp: i32,
    pub StencilFunc: i32,
}

#[repr(C)]
pub(super) struct D3D12_DEPTH_STENCIL_DESC {
    pub DepthEnable: BOOL,
    pub DepthWriteMask: i32,
    pub DepthFunc: i32,
    pub StencilEnable: BOOL,
    pub StencilReadMask: u8,
    pub StencilWriteMask: u8,
    pub FrontFace: D3D12_DEPTH_STENCILOP_DESC,
    pub BackFace: D3D12_DEPTH_STENCILOP_DESC,
}

#[repr(C)]
pub(super) struct D3D12_INPUT_LAYOUT_DESC {
    pub pInputElementDescs: *const c_void,
    pub NumElements: u32,
}

#[repr(C)]
pub(super) struct D3D12_CACHED_PIPELINE_STATE {
    pub pCachedBlob: *const c_void,
    pub CachedBlobSizeInBytes: usize,
}

#[repr(C)]
pub(super) struct D3D12_GRAPHICS_PIPELINE_STATE_DESC {
    pub pRootSignature: *mut c_void,
    pub VS: D3D12_SHADER_BYTECODE,
    pub PS: D3D12_SHADER_BYTECODE,
    pub DS: D3D12_SHADER_BYTECODE,
    pub HS: D3D12_SHADER_BYTECODE,
    pub GS: D3D12_SHADER_BYTECODE,
    pub StreamOutput: D3D12_STREAM_OUTPUT_DESC,
    pub BlendState: D3D12_BLEND_DESC,
    pub SampleMask: u32,
    pub RasterizerState: D3D12_RASTERIZER_DESC,
    pub DepthStencilState: D3D12_DEPTH_STENCIL_DESC,
    pub InputLayout: D3D12_INPUT_LAYOUT_DESC,
    pub IBStripCutValue: i32,
    pub PrimitiveTopologyType: i32,
    pub NumRenderTargets: u32,
    pub RTVFormats: [u32; 8],
    pub DSVFormat: u32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub NodeMask: u32,
    pub CachedPSO: D3D12_CACHED_PIPELINE_STATE,
    pub Flags: u32,
}

#[repr(C)]
pub(super) struct D3D12_COMPUTE_PIPELINE_STATE_DESC {
    pub pRootSignature: *mut c_void,
    pub CS: D3D12_SHADER_BYTECODE,
    pub NodeMask: u32,
    pub CachedPSO: D3D12_CACHED_PIPELINE_STATE,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_RESOURCE_TRANSITION_BARRIER {
    pub pResource: *mut c_void,
    pub Subresource: u32,
    pub StateBefore: u32,
    pub StateAfter: u32,
}

/// The `Transition` member of the union is the largest (24 bytes), so this is the full size.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_RESOURCE_BARRIER {
    pub Type: i32,
    pub Flags: i32,
    pub Transition: D3D12_RESOURCE_TRANSITION_BARRIER,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_BARRIER_SUBRESOURCE_RANGE {
    pub IndexOrFirstMipLevel: u32,
    pub NumMipLevels: u32,
    pub FirstArraySlice: u32,
    pub NumArraySlices: u32,
    pub FirstPlane: u32,
    pub NumPlanes: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_TEXTURE_BARRIER {
    pub SyncBefore: u32,
    pub SyncAfter: u32,
    pub AccessBefore: u32,
    pub AccessAfter: u32,
    pub LayoutBefore: u32,
    pub LayoutAfter: u32,
    pub pResource: *mut c_void,
    pub Subresources: D3D12_BARRIER_SUBRESOURCE_RANGE,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_BUFFER_BARRIER {
    pub SyncBefore: u32,
    pub SyncAfter: u32,
    pub AccessBefore: u32,
    pub AccessAfter: u32,
    pub pResource: *mut c_void,
    pub Offset: u64,
    pub Size: u64,
}

/// The union holds one pointer, whichever barrier type `Type` names.
#[repr(C)]
pub(super) struct D3D12_BARRIER_GROUP {
    pub Type: i32,
    pub NumBarriers: u32,
    pub pBarriers: *const c_void,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS12 {
    pub MSPrimitivesPipelineStatisticIncludesCulledPrimitives: i32,
    pub EnhancedBarriersSupported: BOOL,
    pub RelaxedFormatCastingSupported: BOOL,
}

/// The leading fields of `D3D12_FEATURE_DATA_D3D12_OPTIONS` (15 words in all).
#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS {
    pub DoublePrecisionFloatShaderOps: BOOL,
    pub OutputMergerLogicOp: BOOL,
    pub MinPrecisionSupport: i32,
    pub TiledResourcesTier: i32,
    pub ResourceBindingTier: i32,
    pub _5_13: [u32; 9],
    pub ResourceHeapTier: i32,
}

#[repr(C)]
pub(super) struct D3D12_FEATURE_DATA_SHADER_MODEL {
    pub HighestShaderModel: i32,
}

#[repr(C)]
pub(super) struct D3D12_MESSAGE {
    pub Category: i32,
    pub Severity: i32,
    pub ID: i32,
    pub pDescription: *const u8,
    pub DescriptionByteLength: usize,
}

// --- Entry points ---

pub(super) type D3D12CreateDevice =
    unsafe extern "system" fn(*mut c_void, i32, *const GUID, *mut *mut c_void) -> HRESULT;
pub(super) type D3D12GetDebugInterface =
    unsafe extern "system" fn(*const GUID, *mut *mut c_void) -> HRESULT;
pub(super) type CreateDXGIFactory2 =
    unsafe extern "system" fn(u32, *const GUID, *mut *mut c_void) -> HRESULT;
pub(super) type D3D12SerializeVersionedRootSignature = unsafe extern "system" fn(
    *const D3D12_VERSIONED_ROOT_SIGNATURE_DESC,
    *mut *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

static D3D12_CREATE_DEVICE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static D3D12_SERIALIZE_VERSIONED_ROOT_SIGNATURE: AtomicPtr<c_void> =
    AtomicPtr::new(core::ptr::null_mut());
static D3D12_GET_DEBUG_INTERFACE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static CREATE_DXGI_FACTORY2: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// Resolves `name` in `dll` (from System32), caching it in `cache`. The DLL stays loaded for the
/// life of the process: D3D12 objects may outlive any one instance.
fn proc(cache: &AtomicPtr<c_void>, dll: *const u16, name: PCSTR) -> Option<*mut c_void> {
    let cached = cache.load(Ordering::Acquire);
    if !cached.is_null() {
        return Some(cached);
    }
    // SAFETY: NUL-terminated names; System32 only, so no DLL planting. Racing loads resolve the
    // same address.
    let address = unsafe {
        let module = LoadLibraryExW(dll, core::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        if module.is_null() {
            return None;
        }
        GetProcAddress(module, name)? as *mut c_void
    };
    cache.store(address, Ordering::Release);
    Some(address)
}

pub(super) fn d3d12_create_device() -> Option<D3D12CreateDevice> {
    let p = proc(
        &D3D12_CREATE_DEVICE,
        w!("d3d12.dll"),
        s!("D3D12CreateDevice"),
    )?;
    // SAFETY: the export has this signature.
    Some(unsafe { core::mem::transmute::<*mut c_void, D3D12CreateDevice>(p) })
}

pub(super) fn d3d12_get_debug_interface() -> Option<D3D12GetDebugInterface> {
    let p = proc(
        &D3D12_GET_DEBUG_INTERFACE,
        w!("d3d12.dll"),
        s!("D3D12GetDebugInterface"),
    )?;
    // SAFETY: the export has this signature.
    Some(unsafe { core::mem::transmute::<*mut c_void, D3D12GetDebugInterface>(p) })
}

pub(super) fn d3d12_serialize_versioned_root_signature()
-> Option<D3D12SerializeVersionedRootSignature> {
    let p = proc(
        &D3D12_SERIALIZE_VERSIONED_ROOT_SIGNATURE,
        w!("d3d12.dll"),
        s!("D3D12SerializeVersionedRootSignature"),
    )?;
    // SAFETY: the export has this signature.
    Some(unsafe { core::mem::transmute::<*mut c_void, D3D12SerializeVersionedRootSignature>(p) })
}

pub(super) fn create_dxgi_factory2() -> Option<CreateDXGIFactory2> {
    let p = proc(
        &CREATE_DXGI_FACTORY2,
        w!("dxgi.dll"),
        s!("CreateDXGIFactory2"),
    )?;
    // SAFETY: the export has this signature.
    Some(unsafe { core::mem::transmute::<*mut c_void, CreateDXGIFactory2>(p) })
}

// --- R0: queues, indirect, mesh shaders, queries, heaps, formats (from d3d12.h, dxgiformat.h) ---

pub(super) const D3D12_COMMAND_LIST_TYPE_COMPUTE: i32 = 2;
pub(super) const D3D12_COMMAND_LIST_TYPE_COPY: i32 = 3;

pub(super) const D3D12_FEATURE_D3D12_OPTIONS3: i32 = 21;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS5: i32 = 27;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS7: i32 = 32;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS9: i32 = 37;

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS3 {
    pub CopyQueueTimestampQueriesSupported: BOOL,
    pub CastingFullyTypedFormatSupported: BOOL,
    pub WriteBufferImmediateSupportFlags: u32,
    pub ViewInstancingTier: i32,
    pub BarycentricsSupported: BOOL,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS5 {
    pub SRVOnlyTiledResourceTier3: BOOL,
    pub RenderPassesTier: i32,
    pub RaytracingTier: i32,
}

/// DXR 1.1: inline ray queries.
pub(super) const D3D12_RAYTRACING_TIER_1_1: i32 = 11;

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS7 {
    pub MeshShaderTier: i32,
    pub SamplerFeedbackTier: i32,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS9 {
    pub MeshShaderPipelineStatsSupported: BOOL,
    pub MeshShaderSupportsFullRangeRenderTargetArrayIndex: BOOL,
    pub AtomicInt64OnTypedResourceSupported: BOOL,
    pub AtomicInt64OnGroupSharedSupported: BOOL,
    pub DerivativesInMeshAndAmplificationShadersSupported: BOOL,
    pub WaveMMATier: i32,
}

pub(super) const D3D12_RESOURCE_HEAP_TIER_2: i32 = 2;

// Indirect commands.
pub(super) const D3D12_INDIRECT_ARGUMENT_TYPE_DRAW: i32 = 0;
pub(super) const D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED: i32 = 1;
pub(super) const D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH: i32 = 2;
pub(super) const D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT: i32 = 5;
pub(super) const D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH: i32 = 10;

/// The type and the largest union member (`Constant`: root parameter, offset, count).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_INDIRECT_ARGUMENT_DESC {
    pub Type: i32,
    pub RootParameterIndex: u32,
    pub DestOffsetIn32BitValues: u32,
    pub Num32BitValuesToSet: u32,
}

#[repr(C)]
pub(super) struct D3D12_COMMAND_SIGNATURE_DESC {
    pub ByteStride: u32,
    pub NumArgumentDescs: u32,
    pub pArgumentDescs: *const D3D12_INDIRECT_ARGUMENT_DESC,
    pub NodeMask: u32,
}

// Queries.
pub(super) const D3D12_QUERY_HEAP_TYPE_TIMESTAMP: i32 = 1;
pub(super) const D3D12_QUERY_HEAP_TYPE_COPY_QUEUE_TIMESTAMP: i32 = 5;
pub(super) const D3D12_QUERY_TYPE_TIMESTAMP: i32 = 2;

#[repr(C)]
pub(super) struct D3D12_QUERY_HEAP_DESC {
    pub Type: i32,
    pub Count: u32,
    pub NodeMask: u32,
}

// Heaps and placed resources.
pub(super) const D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS: u32 = 0xc0;
pub(super) const D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES: u32 = 0x44;
pub(super) const D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES: u32 = 0x84;

#[repr(C)]
pub(super) struct D3D12_HEAP_DESC {
    pub SizeInBytes: u64,
    pub Properties: D3D12_HEAP_PROPERTIES,
    pub Alignment: u64,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_RESOURCE_ALLOCATION_INFO {
    pub SizeInBytes: u64,
    pub Alignment: u64,
}

pub(super) const D3D12_RESOURCE_BARRIER_TYPE_ALIASING: i32 = 1;
pub(super) const D3D12_TEXTURE_BARRIER_FLAG_DISCARD: u32 = 0x1;

pub(super) const D3D12_CLEAR_FLAG_STENCIL: u32 = 0x2;

pub(super) const D3D12_FEATURE_D3D12_OPTIONS1: i32 = 8;

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS1 {
    pub WaveOps: BOOL,
    pub WaveLaneCountMin: u32,
    pub WaveLaneCountMax: u32,
    pub TotalLaneCount: u32,
    pub ExpandedComputeResourceStates: BOOL,
    pub Int64ShaderOps: BOOL,
}

pub(super) const D3D12_RESOURCE_DIMENSION_TEXTURE3D: i32 = 4;
pub(super) const D3D12_SRV_DIMENSION_TEXTURE3D: i32 = 8;
pub(super) const D3D12_UAV_DIMENSION_TEXTURE3D: i32 = 8;

// Pipeline state streams.
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE: u32 = 0;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS: u32 = 2;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND: u32 = 8;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK: u32 = 9;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER: u32 = 10;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL: u32 = 11;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS: u32 = 15;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT: u32 = 16;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC: u32 = 17;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_AS: u32 = 24;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS: u32 = 25;

#[repr(C)]
pub(super) struct D3D12_PIPELINE_STATE_STREAM_DESC {
    pub SizeInBytes: usize,
    pub pPipelineStateSubobjectStream: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_RT_FORMAT_ARRAY {
    pub RTFormats: [u32; 8],
    pub NumRenderTargets: u32,
}

// Formats.
pub(super) const DXGI_FORMAT_R32G32B32A32_UINT: u32 = 3;
pub(super) const DXGI_FORMAT_R16G16B16A16_UNORM: u32 = 11;
pub(super) const DXGI_FORMAT_R16G16B16A16_UINT: u32 = 12;
pub(super) const DXGI_FORMAT_R32G32_UINT: u32 = 17;
pub(super) const DXGI_FORMAT_R32G8X24_TYPELESS: u32 = 19;
pub(super) const DXGI_FORMAT_D32_FLOAT_S8X24_UINT: u32 = 20;
pub(super) const DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS: u32 = 21;
pub(super) const DXGI_FORMAT_R16G16_UINT: u32 = 36;
pub(super) const DXGI_FORMAT_R16_UNORM: u32 = 56;
pub(super) const DXGI_FORMAT_R8_UINT: u32 = 62;
pub(super) const DXGI_FORMAT_BC6H_SF16: u32 = 96;

pub(super) const D3D12_DSV_FLAG_READ_ONLY_STENCIL: u32 = 2;
/// Skips zeroing new heaps (transient and pooled memory is always written before it is read).
pub(super) const D3D12_HEAP_FLAG_CREATE_NOT_ZEROED: u32 = 0x1000;
pub(super) const D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT: u64 = 4096;
pub(super) const D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT: u64 = 65536;
