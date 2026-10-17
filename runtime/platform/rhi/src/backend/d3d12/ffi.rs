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
#[derive(Clone, Copy)]
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

pub(super) const D3D12_BARRIER_TYPE_GLOBAL: i32 = 0;
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
#[derive(Clone, Copy)]
pub(super) struct D3D12_BLEND_DESC {
    pub AlphaToCoverageEnable: BOOL,
    pub IndependentBlendEnable: BOOL,
    pub RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
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

/// `D3D12_DEPTH_STENCIL_DESC1`: the classic description plus the depth bounds test.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_DEPTH_STENCIL_DESC1 {
    pub DepthEnable: BOOL,
    pub DepthWriteMask: i32,
    pub DepthFunc: i32,
    pub StencilEnable: BOOL,
    pub StencilReadMask: u8,
    pub StencilWriteMask: u8,
    pub FrontFace: D3D12_DEPTH_STENCILOP_DESC,
    pub BackFace: D3D12_DEPTH_STENCILOP_DESC,
    pub DepthBoundsTestEnable: BOOL,
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

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_GLOBAL_BARRIER {
    pub SyncBefore: u32,
    pub SyncAfter: u32,
    pub AccessBefore: u32,
    pub AccessAfter: u32,
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

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS {
    pub DoublePrecisionFloatShaderOps: BOOL,
    pub OutputMergerLogicOp: BOOL,
    pub MinPrecisionSupport: i32,
    pub TiledResourcesTier: i32,
    pub ResourceBindingTier: i32,
    pub PSSpecifiedStencilRefSupported: BOOL,
    pub TypedUAVLoadAdditionalFormats: BOOL,
    pub ROVsSupported: BOOL,
    pub ConservativeRasterizationTier: i32,
    pub MaxGPUVirtualAddressBitsPerResource: u32,
    pub StandardSwizzle64KBSupported: BOOL,
    pub CrossNodeSharingTier: i32,
    pub CrossAdapterRowMajorTextureSupported: BOOL,
    pub VPAndRTArrayIndexFromAnyShaderFeedingRasterizerSupportedWithoutGSEmulation: BOOL,
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

// --- Production features: formats, MSAA, views, queries, ray queries, sparse, VRS, DRED,
// presentation (from d3d12.h, dxgiformat.h, dxgi1_6.h, winerror.h) ---

pub(super) const DXGI_ERROR_FRAME_STATISTICS_DISJOINT: HRESULT = 0x887A_000B_u32 as i32;

pub(super) const DXGI_FORMAT_R32G32B32A32_SINT: u32 = 4;
pub(super) const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;
pub(super) const DXGI_FORMAT_R16G16B16A16_SNORM: u32 = 13;
pub(super) const DXGI_FORMAT_R16G16B16A16_SINT: u32 = 14;
pub(super) const DXGI_FORMAT_R32G32_SINT: u32 = 18;
pub(super) const DXGI_FORMAT_X32_TYPELESS_G8X24_UINT: u32 = 22;
pub(super) const DXGI_FORMAT_R10G10B10A2_UINT: u32 = 25;
pub(super) const DXGI_FORMAT_R8G8B8A8_UINT: u32 = 30;
pub(super) const DXGI_FORMAT_R8G8B8A8_SNORM: u32 = 31;
pub(super) const DXGI_FORMAT_R8G8B8A8_SINT: u32 = 32;
pub(super) const DXGI_FORMAT_R16G16_UNORM: u32 = 35;
pub(super) const DXGI_FORMAT_R16G16_SNORM: u32 = 37;
pub(super) const DXGI_FORMAT_R16G16_SINT: u32 = 38;
pub(super) const DXGI_FORMAT_R32_SINT: u32 = 43;
pub(super) const DXGI_FORMAT_R8G8_UINT: u32 = 50;
pub(super) const DXGI_FORMAT_R8G8_SNORM: u32 = 51;
pub(super) const DXGI_FORMAT_R8G8_SINT: u32 = 52;
pub(super) const DXGI_FORMAT_R16_TYPELESS: u32 = 53;
pub(super) const DXGI_FORMAT_D16_UNORM: u32 = 55;
pub(super) const DXGI_FORMAT_R16_SNORM: u32 = 58;
pub(super) const DXGI_FORMAT_R16_SINT: u32 = 59;
pub(super) const DXGI_FORMAT_R8_SNORM: u32 = 63;
pub(super) const DXGI_FORMAT_R8_SINT: u32 = 64;
pub(super) const DXGI_FORMAT_R9G9B9E5_SHAREDEXP: u32 = 67;
pub(super) const DXGI_FORMAT_BC2_UNORM: u32 = 74;
pub(super) const DXGI_FORMAT_BC2_UNORM_SRGB: u32 = 75;
pub(super) const DXGI_FORMAT_BC4_SNORM: u32 = 81;
pub(super) const DXGI_FORMAT_BC5_SNORM: u32 = 84;

pub(super) const D3D12_FEATURE_FORMAT_SUPPORT: i32 = 3;
pub(super) const D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS: i32 = 4;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS2: i32 = 18;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS4: i32 = 23;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS6: i32 = 30;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS16: i32 = 45;

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_FORMAT_SUPPORT {
    pub Format: u32,
    pub Support1: u32,
    pub Support2: u32,
}

pub(super) const D3D12_FORMAT_SUPPORT1_TEXTURE2D: u32 = 0x20;
pub(super) const D3D12_FORMAT_SUPPORT1_TEXTURE3D: u32 = 0x40;
pub(super) const D3D12_FORMAT_SUPPORT1_SHADER_LOAD: u32 = 0x100;
pub(super) const D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE: u32 = 0x200;
pub(super) const D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE_COMPARISON: u32 = 0x400;
pub(super) const D3D12_FORMAT_SUPPORT1_RENDER_TARGET: u32 = 0x4000;
pub(super) const D3D12_FORMAT_SUPPORT1_BLENDABLE: u32 = 0x8000;
pub(super) const D3D12_FORMAT_SUPPORT1_DEPTH_STENCIL: u32 = 0x1_0000;
pub(super) const D3D12_FORMAT_SUPPORT1_MULTISAMPLE_RESOLVE: u32 = 0x4_0000;
pub(super) const D3D12_FORMAT_SUPPORT1_MULTISAMPLE_LOAD: u32 = 0x40_0000;
pub(super) const D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW: u32 = 0x200_0000;
pub(super) const D3D12_FORMAT_SUPPORT2_UAV_ATOMIC_ADD: u32 = 0x1;
pub(super) const D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD: u32 = 0x40;
pub(super) const D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE: u32 = 0x80;
pub(super) const D3D12_FORMAT_SUPPORT2_TILED: u32 = 0x200;

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS {
    pub Format: u32,
    pub SampleCount: u32,
    pub Flags: u32,
    pub NumQualityLevels: u32,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS2 {
    pub DepthBoundsTestSupported: BOOL,
    pub ProgrammableSamplePositionsTier: i32,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS4 {
    pub MSAA64KBAlignedTextureSupported: BOOL,
    pub SharedResourceCompatibilityTier: i32,
    pub Native16BitShaderOpsSupported: BOOL,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS6 {
    pub AdditionalShadingRatesSupported: BOOL,
    pub PerPrimitiveShadingRateSupportedWithViewportIndexing: BOOL,
    pub VariableShadingRateTier: i32,
    pub ShadingRateImageTileSize: u32,
    pub BackgroundProcessingSupported: BOOL,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct D3D12_FEATURE_DATA_D3D12_OPTIONS16 {
    pub DynamicDepthBiasSupported: BOOL,
    pub GPUUploadHeapSupported: BOOL,
}

pub(super) const D3D12_TILED_RESOURCES_TIER_2: i32 = 2;
pub(super) const D3D12_TILED_RESOURCES_TIER_3: i32 = 3;

// Views.
pub(super) const D3D12_RTV_DIMENSION_TEXTURE2DMS: i32 = 6;
pub(super) const D3D12_RTV_DIMENSION_TEXTURE2DMSARRAY: i32 = 7;
pub(super) const D3D12_DSV_DIMENSION_TEXTURE2DMS: i32 = 5;
pub(super) const D3D12_DSV_DIMENSION_TEXTURE2DMSARRAY: i32 = 6;
pub(super) const D3D12_SRV_DIMENSION_TEXTURE2DMS: i32 = 6;
pub(super) const D3D12_SRV_DIMENSION_TEXTURE2DMSARRAY: i32 = 7;
pub(super) const D3D12_SRV_DIMENSION_TEXTURECUBEARRAY: i32 = 10;
pub(super) const D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE: i32 = 11;

/// `D3D12_ENCODE_SHADER_4_COMPONENT_MAPPING(1, 0, 0, 1)` with forced zeros and one: the stencil
/// plane (in green for `X32_TYPELESS_G8X24_UINT`) read as red.
pub(super) const STENCIL_IN_RED_COMPONENT_MAPPING: u32 =
    1 | (4 << 3) | (4 << 6) | (5 << 9) | (1 << 12);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_GPU_DESCRIPTOR_HANDLE {
    pub ptr: u64,
}

// Resource states and layouts.
pub(super) const D3D12_RESOURCE_STATE_PREDICATION: u32 = 0x200;
pub(super) const D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE: u32 = 0x40_0000;
pub(super) const D3D12_RESOURCE_STATE_SHADING_RATE_SOURCE: u32 = 0x100_0000;
pub(super) const D3D12_BARRIER_LAYOUT_SHADING_RATE_SOURCE: u32 = 11;

// Heaps and sharing.
pub(super) const D3D12_HEAP_TYPE_GPU_UPLOAD: i32 = 5;
pub(super) const D3D12_HEAP_FLAG_SHARED: u32 = 0x1;
pub(super) const D3D12_FENCE_FLAG_SHARED: i32 = 0x1;
/// `GENERIC_ALL`, the access shared handles are created with.
pub(super) const GENERIC_ALL: u32 = 0x1000_0000;
pub(super) const D3D12_MULTIPLE_FENCE_WAIT_FLAG_ANY: i32 = 0x1;

// Queries and predication.
pub(super) const D3D12_QUERY_HEAP_TYPE_OCCLUSION: i32 = 0;
pub(super) const D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1: i32 = 7;
pub(super) const D3D12_QUERY_TYPE_OCCLUSION: i32 = 0;
pub(super) const D3D12_QUERY_TYPE_BINARY_OCCLUSION: i32 = 1;
pub(super) const D3D12_QUERY_TYPE_PIPELINE_STATISTICS1: i32 = 10;
pub(super) const D3D12_PREDICATION_OP_EQUAL_ZERO: i32 = 0;
pub(super) const D3D12_PREDICATION_OP_NOT_EQUAL_ZERO: i32 = 1;

// Markers.
pub(super) const D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_IN: i32 = 0x1;
pub(super) const D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_OUT: i32 = 0x2;
pub(super) const D3D12_COMMAND_LIST_SUPPORT_FLAG_DIRECT: u32 = 0x1;
pub(super) const D3D12_COMMAND_LIST_SUPPORT_FLAG_COMPUTE: u32 = 0x4;
pub(super) const D3D12_COMMAND_LIST_SUPPORT_FLAG_COPY: u32 = 0x8;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_WRITEBUFFERIMMEDIATE_PARAMETER {
    pub Dest: u64,
    pub Value: u32,
}

// Pipelines.
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_VS: u32 = 1;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_CS: u32 = 6;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_IB_STRIP_CUT_VALUE: u32 = 13;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY: u32 = 14;
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1: u32 = 21;
pub(super) const D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED: i32 = 0;
pub(super) const D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_0XFFFF: i32 = 1;
pub(super) const D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_0XFFFFFFFF: i32 = 2;
pub(super) const D3D12_CONSERVATIVE_RASTERIZATION_MODE_ON: i32 = 1;
pub(super) const D3D12_FILTER_REDUCTION_TYPE_MINIMUM: i32 = 2;
pub(super) const D3D12_FILTER_REDUCTION_TYPE_MAXIMUM: i32 = 3;

// Residency.
pub(super) const D3D12_RESIDENCY_PRIORITY_MINIMUM: i32 = 0x2800_0000;
pub(super) const D3D12_RESIDENCY_PRIORITY_LOW: i32 = 0x5000_0000;
pub(super) const D3D12_RESIDENCY_PRIORITY_NORMAL: i32 = 0x7800_0000;
pub(super) const D3D12_RESIDENCY_PRIORITY_HIGH: i32 = 0xa001_0000_u32 as i32;
pub(super) const D3D12_RESIDENCY_PRIORITY_MAXIMUM: i32 = 0xc800_0000_u32 as i32;

// Variable-rate shading.
pub(super) const D3D12_VARIABLE_SHADING_RATE_TIER_1: i32 = 1;
pub(super) const D3D12_VARIABLE_SHADING_RATE_TIER_2: i32 = 2;

// Sparse (tiled) resources.
pub(super) const D3D12_TILE_RANGE_FLAG_NULL: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_TILED_RESOURCE_COORDINATE {
    pub X: u32,
    pub Y: u32,
    pub Z: u32,
    pub Subresource: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_TILE_REGION_SIZE {
    pub NumTiles: u32,
    pub UseBox: BOOL,
    pub Width: u32,
    pub Height: u16,
    pub Depth: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_TILE_SHAPE {
    pub WidthInTexels: u32,
    pub HeightInTexels: u32,
    pub DepthInTexels: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_PACKED_MIP_INFO {
    pub NumStandardMips: u8,
    pub NumPackedMips: u8,
    pub NumTilesForPackedMips: u32,
    pub StartTileIndexInOverallResource: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_SUBRESOURCE_TILING {
    pub WidthInTiles: u32,
    pub HeightInTiles: u16,
    pub DepthInTiles: u16,
    pub StartTileIndexInOverallResource: u32,
}

// Acceleration structures.
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL: i32 = 0;
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL: i32 = 1;
pub(super) const D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES: i32 = 0;
pub(super) const D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS: i32 = 1;
pub(super) const D3D12_ELEMENTS_LAYOUT_ARRAY: i32 = 0;
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE: u32 = 0x20;
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE: i32 = 0;
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT: i32 = 1;
pub(super) const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE: i32 = 0;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
    pub StartAddress: u64,
    pub StrideInBytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
    pub Transform3x4: u64,
    pub IndexFormat: u32,
    pub VertexFormat: u32,
    pub IndexCount: u32,
    pub VertexCount: u32,
    pub IndexBuffer: u64,
    pub VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE,
}

/// The union's AABB member overlays the triangles member's first 24 bytes: `AABBCount` on
/// `Transform3x4`, the address-and-stride on the next four words.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
    pub AABBCount: u64,
    pub AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE,
}

/// `Triangles` is the union's largest member (48 bytes); `aabbs` reinterprets it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_RAYTRACING_GEOMETRY_DESC {
    pub Type: i32,
    pub Flags: u32,
    pub Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC,
}

impl D3D12_RAYTRACING_GEOMETRY_DESC {
    pub fn aabbs(flags: u32, aabbs: D3D12_RAYTRACING_GEOMETRY_AABBS_DESC) -> Self {
        let mut desc = Self {
            Type: D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS,
            Flags: flags,
            ..Default::default()
        };
        // SAFETY: the AABB description is smaller than the triangles one and both are plain
        // data in the same union slot.
        unsafe {
            core::ptr::write(
                (&raw mut desc.Triangles).cast::<D3D12_RAYTRACING_GEOMETRY_AABBS_DESC>(),
                aabbs,
            )
        };
        desc
    }
}

/// The union holds `InstanceDescs` (a GPU address) or `pGeometryDescs` (a pointer): one 8-byte
/// word.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
    pub Type: i32,
    pub Flags: u32,
    pub NumDescs: u32,
    pub DescsLayout: i32,
    pub Descs: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
    pub DestAccelerationStructureData: u64,
    pub Inputs: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
    pub SourceAccelerationStructureData: u64,
    pub ScratchAccelerationStructureData: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO {
    pub ResultDataMaxSizeInBytes: u64,
    pub ScratchDataSizeInBytes: u64,
    pub UpdateScratchDataSizeInBytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
    pub DestBuffer: u64,
    pub InfoType: i32,
}

// Device removed extended data (DRED).
pub(super) const D3D12_DRED_ENABLEMENT_FORCED_ON: i32 = 2;

#[repr(C)]
pub(super) struct D3D12_AUTO_BREADCRUMB_NODE {
    pub pCommandListDebugNameA: *const u8,
    pub pCommandListDebugNameW: *const u16,
    pub pCommandQueueDebugNameA: *const u8,
    pub pCommandQueueDebugNameW: *const u16,
    pub pCommandList: *mut c_void,
    pub pCommandQueue: *mut c_void,
    pub BreadcrumbCount: u32,
    pub pLastBreadcrumbValue: *const u32,
    pub pCommandHistory: *const i32,
    pub pNext: *const D3D12_AUTO_BREADCRUMB_NODE,
}

#[repr(C)]
pub(super) struct D3D12_DRED_ALLOCATION_NODE {
    pub ObjectNameA: *const u8,
    pub ObjectNameW: *const u16,
    pub AllocationType: i32,
    pub pNext: *const D3D12_DRED_ALLOCATION_NODE,
}

#[repr(C)]
pub(super) struct D3D12_DRED_AUTO_BREADCRUMBS_OUTPUT {
    pub pHeadAutoBreadcrumbNode: *const D3D12_AUTO_BREADCRUMB_NODE,
}

#[repr(C)]
pub(super) struct D3D12_DRED_PAGE_FAULT_OUTPUT {
    pub PageFaultVA: u64,
    pub pHeadExistingAllocationNode: *const D3D12_DRED_ALLOCATION_NODE,
    pub pHeadRecentFreedAllocationNode: *const D3D12_DRED_ALLOCATION_NODE,
}

// DXGI: memory budget, outputs, presentation statistics, HDR.
pub(super) const DXGI_MEMORY_SEGMENT_GROUP_LOCAL: i32 = 0;
pub(super) const DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL: i32 = 1;
pub(super) const DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709: i32 = 0;
pub(super) const DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709: i32 = 1;
pub(super) const DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: i32 = 12;
pub(super) const DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT: u32 = 0x1;
pub(super) const DXGI_HDR_METADATA_TYPE_NONE: i32 = 0;
pub(super) const DXGI_HDR_METADATA_TYPE_HDR10: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct DXGI_QUERY_VIDEO_MEMORY_INFO {
    pub Budget: u64,
    pub CurrentUsage: u64,
    pub AvailableForReservation: u64,
    pub CurrentReservation: u64,
}

#[repr(C)]
pub(super) struct DXGI_OUTPUT_DESC1 {
    pub DeviceName: [u16; 32],
    pub DesktopCoordinates: windows_sys::Win32::Foundation::RECT,
    pub AttachedToDesktop: BOOL,
    pub Rotation: i32,
    pub Monitor: *mut c_void,
    pub BitsPerColor: u32,
    pub ColorSpace: i32,
    pub RedPrimary: [f32; 2],
    pub GreenPrimary: [f32; 2],
    pub BluePrimary: [f32; 2],
    pub WhitePoint: [f32; 2],
    pub MinLuminance: f32,
    pub MaxLuminance: f32,
    pub MaxFullFrameLuminance: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct DXGI_FRAME_STATISTICS {
    pub PresentCount: u32,
    pub PresentRefreshCount: u32,
    pub SyncRefreshCount: u32,
    pub SyncQPCTime: i64,
    pub SyncGPUTime: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct DXGI_HDR_METADATA_HDR10 {
    pub RedPrimary: [u16; 2],
    pub GreenPrimary: [u16; 2],
    pub BluePrimary: [u16; 2],
    pub WhitePoint: [u16; 2],
    pub MaxMasteringLuminance: u32,
    pub MinMasteringLuminance: u32,
    pub MaxContentLightLevel: u16,
    pub MaxFrameAverageLightLevel: u16,
}

// Cached pipeline blobs.
pub(super) const D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_CACHED_PSO: u32 = 19;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct D3D12_CACHED_PIPELINE_STATE {
    pub pCachedBlob: *const c_void,
    pub CachedBlobSizeInBytes: usize,
}

// Resources created with barrier layouts (ID3D12Device10).
pub(super) const D3D12_RESOURCE_FLAG_RAYTRACING_ACCELERATION_STRUCTURE: u32 = 0x100;

/// `D3D12_RESOURCE_DESC` with `D3D12_MIP_REGION` appended in place (flat: a nested struct would
/// add tail padding before it).
#[repr(C)]
pub(super) struct D3D12_RESOURCE_DESC1 {
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
    /// Width, height, depth; 0 without sampler feedback.
    pub SamplerFeedbackMipRegion: [u32; 3],
}

const _: () = assert!(size_of::<D3D12_RESOURCE_DESC1>() == 64);
