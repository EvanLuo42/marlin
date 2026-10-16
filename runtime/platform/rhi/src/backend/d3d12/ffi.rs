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
pub(super) const DXGI_FORMAT_R16G16B16A16_FLOAT: u32 = 10;
pub(super) const DXGI_FORMAT_R10G10B10A2_UNORM: u32 = 24;
pub(super) const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
pub(super) const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = 29;
pub(super) const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
pub(super) const DXGI_FORMAT_B8G8R8A8_UNORM_SRGB: u32 = 91;

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
pub(super) const D3D12_COMMAND_LIST_TYPE_DIRECT: i32 = 0;
pub(super) const D3D12_DESCRIPTOR_HEAP_TYPE_RTV: i32 = 2;
pub(super) const D3D12_RTV_DIMENSION_TEXTURE2D: i32 = 4;
pub(super) const D3D12_FEATURE_D3D12_OPTIONS12: i32 = 41;
pub(super) const D3D12_MESSAGE_SEVERITY_CORRUPTION: i32 = 0;
pub(super) const D3D12_MESSAGE_SEVERITY_ERROR: i32 = 1;
pub(super) const D3D12_MESSAGE_SEVERITY_WARNING: i32 = 2;

pub(super) const D3D12_RESOURCE_BARRIER_TYPE_TRANSITION: i32 = 0;
pub(super) const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 = 0xffff_ffff;
pub(super) const D3D12_RESOURCE_STATE_COMMON: u32 = 0;
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

/// Only the `Texture2D` member of the union (`MipSlice`, `PlaneSlice`) is used; `_union` pads to
/// the union's 16 bytes, 8-aligned (its `D3D12_BUFFER_RTV` member has a `UINT64`).
#[repr(C)]
pub(super) struct D3D12_RENDER_TARGET_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: i32,
    pub Texture2D: [u32; 2],
    pub _union: [u32; 2],
    pub _align: [u64; 0],
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

static D3D12_CREATE_DEVICE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
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

pub(super) fn create_dxgi_factory2() -> Option<CreateDXGIFactory2> {
    let p = proc(
        &CREATE_DXGI_FACTORY2,
        w!("dxgi.dll"),
        s!("CreateDXGIFactory2"),
    )?;
    // SAFETY: the export has this signature.
    Some(unsafe { core::mem::transmute::<*mut c_void, CreateDXGIFactory2>(p) })
}
