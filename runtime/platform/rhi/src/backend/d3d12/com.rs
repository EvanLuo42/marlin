//! Hand-written COM bindings for the D3D12 and DXGI interfaces the backend calls.
//!
//! windows-sys has no D3D12 or DXGI bindings, and the `windows` crate would bring a large
//! generated API for the few dozen methods used here. Writing them by hand keeps the dependency
//! surface at windows-sys, works in no_std, makes the struct-return ABI quirk explicit
//! (`GetCPUDescriptorHandleForHeapStart` returns through a hidden pointer in C++), and gives the
//! Xbox backend a module it can extend with its own `d3d12_x` interfaces.
//!
//! Vtable layouts come from the C declarations (`<Interface>Vtbl`) in the Windows SDK 10.0.26100
//! headers `d3d12.h`, `d3d12sdklayers.h` and `dxgi1_6.h`. Slots the backend does not call are
//! padding (`[usize; N]`, commented with their slot range); derived interfaces embed their
//! parent's vtable, so a `Com<I>` can only reach methods its interface version has.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use windows_sys::Win32::Foundation::{HANDLE, HWND};
use windows_sys::core::{BOOL, GUID, HRESULT};

use super::ffi::*;

type This = *mut c_void;

/// A COM interface: its IID and vtable type.
///
/// # Safety
/// `IID` and `Vtbl` must match the interface's real definition.
pub(super) unsafe trait Interface {
    const IID: GUID;
    type Vtbl;
}

/// An owned interface pointer: `AddRef` on clone, `Release` on drop.
#[repr(transparent)]
pub(super) struct Com<I: Interface> {
    ptr: NonNull<c_void>,
    _interface: PhantomData<I>,
}

// SAFETY: D3D12 and DXGI objects are free-threaded. The non-thread-safe ones (command lists,
// allocators) are only reached through `&mut` methods of their owners.
unsafe impl<I: Interface> Send for Com<I> {}
// SAFETY: as above.
unsafe impl<I: Interface> Sync for Com<I> {}

impl<I: Interface> Com<I> {
    /// Takes ownership of one reference.
    ///
    /// # Safety
    /// `ptr` is null or a valid `I` pointer whose reference the caller transfers.
    pub(super) unsafe fn from_raw(ptr: *mut c_void) -> Option<Self> {
        NonNull::new(ptr).map(|ptr| Self {
            ptr,
            _interface: PhantomData,
        })
    }

    #[inline]
    pub(super) fn as_raw(&self) -> *mut c_void {
        self.ptr.as_ptr()
    }

    #[inline]
    pub(super) fn vtbl(&self) -> &I::Vtbl {
        // SAFETY: a COM object starts with a pointer to its vtable, which lives as long as the
        // object, and `I::Vtbl` matches it (`Interface` contract).
        unsafe { &**self.ptr.as_ptr().cast::<*const I::Vtbl>() }
    }

    fn unknown(&self) -> &IUnknownVtbl {
        // SAFETY: every vtable starts with IUnknown's three slots.
        unsafe { &**self.ptr.as_ptr().cast::<*const IUnknownVtbl>() }
    }

    /// `QueryInterface`: the same object as interface `J`.
    pub(super) fn cast<J: Interface>(&self) -> Result<Com<J>, HRESULT> {
        let mut out = core::ptr::null_mut();
        // SAFETY: valid object, IID and out pointer.
        let hr = unsafe { (self.unknown().QueryInterface)(self.as_raw(), &J::IID, &mut out) };
        if hr < 0 {
            return Err(hr);
        }
        // SAFETY: QueryInterface succeeded and returned an owned `J` reference.
        unsafe { Com::from_raw(out) }.ok_or(E_POINTER)
    }
}

impl<I: Interface> Clone for Com<I> {
    fn clone(&self) -> Self {
        // SAFETY: valid object.
        unsafe { (self.unknown().AddRef)(self.as_raw()) };
        Self {
            ptr: self.ptr,
            _interface: PhantomData,
        }
    }
}

impl<I: Interface> Drop for Com<I> {
    fn drop(&mut self) {
        // SAFETY: releases the reference this value owns.
        unsafe { (self.unknown().Release)(self.as_raw()) };
    }
}

/// Calls `create(iid, &mut out)` and wraps the result. For every API that returns an interface
/// through `REFIID riid, void** ppv`.
pub(super) fn create<I: Interface>(
    create: impl FnOnce(*const GUID, *mut *mut c_void) -> HRESULT,
) -> Result<Com<I>, HRESULT> {
    let mut out = core::ptr::null_mut();
    let hr = create(&I::IID, &mut out);
    if hr < 0 {
        return Err(hr);
    }
    // SAFETY: success returns an owned reference to an object of the requested IID.
    unsafe { Com::from_raw(out) }.ok_or(E_POINTER)
}

macro_rules! interface {
    ($name:ident, $vtbl:ident, $iid:literal) => {
        pub(super) struct $name;
        // SAFETY: IID and vtable transcribed from the Windows SDK headers.
        unsafe impl Interface for $name {
            const IID: GUID = GUID::from_u128($iid);
            type Vtbl = $vtbl;
        }
    };
}

#[repr(C)]
pub(super) struct IUnknownVtbl {
    pub QueryInterface: unsafe extern "system" fn(This, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(This) -> u32,
    pub Release: unsafe extern "system" fn(This) -> u32,
}

// --- DXGI ---

/// IUnknown + IDXGIObject (3..=6).
#[repr(C)]
pub(super) struct DxgiObjectVtbl {
    pub base: IUnknownVtbl,
    _3_6: [usize; 4],
}

#[repr(C)]
pub(super) struct IDXGIFactory2Vtbl {
    pub base: DxgiObjectVtbl,
    _7: usize,
    pub MakeWindowAssociation: unsafe extern "system" fn(This, HWND, u32) -> HRESULT,
    _9_11: [usize; 3],
    pub EnumAdapters1: unsafe extern "system" fn(This, u32, *mut *mut c_void) -> HRESULT,
    _13_14: [usize; 2],
    pub CreateSwapChainForHwnd: unsafe extern "system" fn(
        This,
        *mut c_void,
        HWND,
        *const DXGI_SWAP_CHAIN_DESC1,
        *const c_void,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
}

#[repr(C)]
pub(super) struct IDXGIFactory4Vtbl {
    pub base: IDXGIFactory2Vtbl,
    _16_26: [usize; 11],
    pub EnumWarpAdapter: unsafe extern "system" fn(This, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
pub(super) struct IDXGIFactory5Vtbl {
    pub base: IDXGIFactory4Vtbl,
    pub CheckFeatureSupport: unsafe extern "system" fn(This, i32, *mut c_void, u32) -> HRESULT,
}

#[repr(C)]
pub(super) struct IDXGIFactory6Vtbl {
    pub base: IDXGIFactory5Vtbl,
    pub EnumAdapterByGpuPreference:
        unsafe extern "system" fn(This, u32, i32, *const GUID, *mut *mut c_void) -> HRESULT,
}

interface!(
    IDXGIFactory4,
    IDXGIFactory4Vtbl,
    0x1bc6ea02_ef36_464f_bf0c_21ca39e5168a
);
interface!(
    IDXGIFactory5,
    IDXGIFactory5Vtbl,
    0x7632e1f5_ee65_4dca_87fd_84cd75f8838d
);
interface!(
    IDXGIFactory6,
    IDXGIFactory6Vtbl,
    0xc1b6694f_ff09_44a9_b03c_77900a0a1d17
);

#[repr(C)]
pub(super) struct IDXGIAdapter1Vtbl {
    pub base: DxgiObjectVtbl,
    _7_9: [usize; 3],
    pub GetDesc1: unsafe extern "system" fn(This, *mut DXGI_ADAPTER_DESC1) -> HRESULT,
}

interface!(
    IDXGIAdapter1,
    IDXGIAdapter1Vtbl,
    0x29038f61_3839_4626_91fd_086879011a05
);

#[repr(C)]
pub(super) struct IDXGISwapChain3Vtbl {
    pub base: DxgiObjectVtbl,
    _7: usize,
    pub Present: unsafe extern "system" fn(This, u32, u32) -> HRESULT,
    pub GetBuffer: unsafe extern "system" fn(This, u32, *const GUID, *mut *mut c_void) -> HRESULT,
    _10_12: [usize; 3],
    pub ResizeBuffers: unsafe extern "system" fn(This, u32, u32, u32, u32, u32) -> HRESULT,
    _14_30: [usize; 17],
    pub SetMaximumFrameLatency: unsafe extern "system" fn(This, u32) -> HRESULT,
    _32: usize,
    pub GetFrameLatencyWaitableObject: unsafe extern "system" fn(This) -> HANDLE,
    _34_35: [usize; 2],
    pub GetCurrentBackBufferIndex: unsafe extern "system" fn(This) -> u32,
}

/// Only cast from (to `IDXGISwapChain3`); never called through.
pub(super) struct IDXGISwapChain1;
// SAFETY: IID from dxgi1_2.h; the vtable is never used.
unsafe impl Interface for IDXGISwapChain1 {
    const IID: GUID = GUID::from_u128(0x790a45f7_0d42_4876_983a_0a55cfe6f4aa);
    type Vtbl = IUnknownVtbl;
}

interface!(
    IDXGISwapChain3,
    IDXGISwapChain3Vtbl,
    0x94d99bdb_f1f8_4ab0_b236_7da0170edab1
);

// --- D3D12 ---

/// IUnknown + ID3D12Object (3..=6) + ID3D12DeviceChild (7).
#[repr(C)]
pub(super) struct DeviceChildVtbl {
    pub base: IUnknownVtbl,
    _3_5: [usize; 3],
    pub SetName: unsafe extern "system" fn(This, *const u16) -> HRESULT,
    _7: usize,
}

#[repr(C)]
pub(super) struct ID3D12DeviceVtbl {
    pub base: IUnknownVtbl,
    _3_7: [usize; 5],
    pub CreateCommandQueue: unsafe extern "system" fn(
        This,
        *const D3D12_COMMAND_QUEUE_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateCommandAllocator:
        unsafe extern "system" fn(This, i32, *const GUID, *mut *mut c_void) -> HRESULT,
    _10_11: [usize; 2],
    pub CreateCommandList: unsafe extern "system" fn(
        This,
        u32,
        i32,
        *mut c_void,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CheckFeatureSupport: unsafe extern "system" fn(This, i32, *mut c_void, u32) -> HRESULT,
    pub CreateDescriptorHeap: unsafe extern "system" fn(
        This,
        *const D3D12_DESCRIPTOR_HEAP_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub GetDescriptorHandleIncrementSize: unsafe extern "system" fn(This, i32) -> u32,
    _16_19: [usize; 4],
    pub CreateRenderTargetView: unsafe extern "system" fn(
        This,
        *mut c_void,
        *const D3D12_RENDER_TARGET_VIEW_DESC,
        D3D12_CPU_DESCRIPTOR_HANDLE,
    ),
    _21_35: [usize; 15],
    pub CreateFence:
        unsafe extern "system" fn(This, u64, i32, *const GUID, *mut *mut c_void) -> HRESULT,
    pub GetDeviceRemovedReason: unsafe extern "system" fn(This) -> HRESULT,
}

interface!(
    ID3D12Device,
    ID3D12DeviceVtbl,
    0x189819f1_1db6_4b57_be54_1821339b85f7
);

#[repr(C)]
pub(super) struct ID3D12CommandQueueVtbl {
    pub base: DeviceChildVtbl,
    _8_9: [usize; 2],
    pub ExecuteCommandLists: unsafe extern "system" fn(This, u32, *const *mut c_void),
    _11_13: [usize; 3],
    pub Signal: unsafe extern "system" fn(This, *mut c_void, u64) -> HRESULT,
}

interface!(
    ID3D12CommandQueue,
    ID3D12CommandQueueVtbl,
    0x0ec870a6_5d7e_4c22_8cfc_5baae07616ed
);

#[repr(C)]
pub(super) struct ID3D12CommandAllocatorVtbl {
    pub base: DeviceChildVtbl,
    pub Reset: unsafe extern "system" fn(This) -> HRESULT,
}

interface!(
    ID3D12CommandAllocator,
    ID3D12CommandAllocatorVtbl,
    0x6102dee4_af59_4b09_b999_b44d73f09b24
);

#[repr(C)]
pub(super) struct ID3D12GraphicsCommandListVtbl {
    pub base: DeviceChildVtbl,
    _8: usize,
    pub Close: unsafe extern "system" fn(This) -> HRESULT,
    pub Reset: unsafe extern "system" fn(This, *mut c_void, *mut c_void) -> HRESULT,
    _11_25: [usize; 15],
    pub ResourceBarrier: unsafe extern "system" fn(This, u32, *const D3D12_RESOURCE_BARRIER),
    _27_45: [usize; 19],
    pub OMSetRenderTargets: unsafe extern "system" fn(
        This,
        u32,
        *const D3D12_CPU_DESCRIPTOR_HANDLE,
        BOOL,
        *const D3D12_CPU_DESCRIPTOR_HANDLE,
    ),
    _47: usize,
    pub ClearRenderTargetView: unsafe extern "system" fn(
        This,
        D3D12_CPU_DESCRIPTOR_HANDLE,
        *const f32,
        u32,
        *const c_void,
    ),
    _49_59: [usize; 11],
}

interface!(
    ID3D12GraphicsCommandList,
    ID3D12GraphicsCommandListVtbl,
    0x5b160d0f_ac1b_4185_8ba8_b3ae42a5a455
);

#[repr(C)]
pub(super) struct ID3D12GraphicsCommandList7Vtbl {
    pub base: ID3D12GraphicsCommandListVtbl,
    /// ID3D12GraphicsCommandList1 through 6.
    _60_79: [usize; 20],
    pub Barrier: unsafe extern "system" fn(This, u32, *const D3D12_BARRIER_GROUP),
}

interface!(
    ID3D12GraphicsCommandList7,
    ID3D12GraphicsCommandList7Vtbl,
    0xdd171223_8b61_4769_90e3_160ccde4e2c1
);

#[repr(C)]
pub(super) struct ID3D12FenceVtbl {
    pub base: DeviceChildVtbl,
    pub GetCompletedValue: unsafe extern "system" fn(This) -> u64,
    pub SetEventOnCompletion: unsafe extern "system" fn(This, u64, HANDLE) -> HRESULT,
    pub Signal: unsafe extern "system" fn(This, u64) -> HRESULT,
}

interface!(
    ID3D12Fence,
    ID3D12FenceVtbl,
    0x0a753dcf_c4d8_4b91_adf6_be5a60d95a76
);

#[repr(C)]
pub(super) struct ID3D12DescriptorHeapVtbl {
    pub base: DeviceChildVtbl,
    _8: usize,
    /// C++ returns the struct through a hidden pointer after `this`.
    pub GetCPUDescriptorHandleForHeapStart:
        unsafe extern "system" fn(
            This,
            *mut D3D12_CPU_DESCRIPTOR_HANDLE,
        ) -> *mut D3D12_CPU_DESCRIPTOR_HANDLE,
}

interface!(
    ID3D12DescriptorHeap,
    ID3D12DescriptorHeapVtbl,
    0x8efb471d_616c_4f49_90f7_127bb763fa51
);

interface!(
    ID3D12Resource,
    DeviceChildVtbl,
    0x696442be_a72e_4059_bc79_5b5c98040fad
);

#[repr(C)]
pub(super) struct ID3D12DebugVtbl {
    pub base: IUnknownVtbl,
    pub EnableDebugLayer: unsafe extern "system" fn(This),
}

interface!(
    ID3D12Debug,
    ID3D12DebugVtbl,
    0x344488b7_6846_474b_b989_f027448245e0
);

#[repr(C)]
pub(super) struct ID3D12InfoQueueVtbl {
    pub base: IUnknownVtbl,
    _3: usize,
    pub ClearStoredMessages: unsafe extern "system" fn(This),
    pub GetMessage: unsafe extern "system" fn(This, u64, *mut D3D12_MESSAGE, *mut usize) -> HRESULT,
    _6_8: [usize; 3],
    pub GetNumStoredMessagesAllowedByRetrievalFilter: unsafe extern "system" fn(This) -> u64,
}

interface!(
    ID3D12InfoQueue,
    ID3D12InfoQueueVtbl,
    0x0742a90b_c387_483f_b946_30a7e4e61458
);
