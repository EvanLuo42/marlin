//! Diagnostics: the debug layer and GPU-based validation, DRED (device removed extended data),
//! device-loss detection with a one-time crash report, and debug names.
//!
//! DRED records an auto-breadcrumb per command-list operation and tracks GPU virtual address
//! allocations, so that when the device is lost the report can say which lists were running,
//! which operation each had reached, and what lived at a faulting address. Debug names
//! (`SetName`) make those reports readable.

use core::fmt::Write as _;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use marlin_sync::Once;
use windows_sys::core::HRESULT;

use super::com::{self, *};
use super::ffi::*;
use crate::{Error, ErrorKind, Result};

// --- Process-wide switches ---

/// Done once per process (enabled or unavailable); threads that arrive meanwhile wait for the
/// attempt to finish, since a device created meanwhile would miss the setting (or be removed by
/// the debug layer).
static DEBUG_LAYER: Once = Once::new();
static GPU_VALIDATION: AtomicBool = AtomicBool::new(false);
static DRED: Once = Once::new();

/// Enables the debug layer (and GPU-based validation) once per process, before any device that
/// should be validated.
pub(super) fn enable_debug_layer(gpu_validation: bool) {
    let mut ran = false;
    DEBUG_LAYER.call_once(|| {
        ran = true;
        let Some(get) = d3d12_get_debug_interface() else {
            return;
        };
        // SAFETY: valid IID and out pointer.
        match com::create::<ID3D12Debug>(|iid, out| unsafe { get(iid, out) }) {
            Ok(debug) => {
                // SAFETY: valid interface.
                unsafe { (debug.vtbl().EnableDebugLayer)(debug.as_raw()) };
                if gpu_validation {
                    match debug.cast::<ID3D12Debug1>() {
                        Ok(debug1) => {
                            // SAFETY: valid interface; before any device exists.
                            unsafe {
                                (debug1.vtbl().SetEnableGPUBasedValidation)(debug1.as_raw(), 1)
                            };
                            GPU_VALIDATION.store(true, Ordering::Relaxed);
                        }
                        Err(_) => marlin_diag::warn!("D3D12 GPU-based validation unavailable"),
                    }
                }
            }
            Err(_) => marlin_diag::warn!(
                "D3D12 debug layer unavailable (install the Graphics Tools optional feature)"
            ),
        }
    });
    if !ran && gpu_validation && !GPU_VALIDATION.load(Ordering::Relaxed) {
        marlin_diag::warn!(
            "D3D12 GPU-based validation must be requested by the first validating instance"
        );
    }
}

/// Turns on DRED auto-breadcrumbs and page-fault tracking once per process, before the first
/// device.
pub(super) fn enable_crash_diagnostics() {
    DRED.call_once(|| {
        let Some(get) = d3d12_get_debug_interface() else {
            return;
        };
        // SAFETY: valid IID and out pointer. The DRED settings interface is part of the core
        // runtime (no Graphics Tools needed).
        match com::create::<ID3D12DeviceRemovedExtendedDataSettings>(|iid, out| unsafe {
            get(iid, out)
        }) {
            Ok(settings) => {
                let s = &settings;
                // SAFETY: valid interface; before any device exists.
                unsafe {
                    (s.vtbl().SetAutoBreadcrumbsEnablement)(
                        s.as_raw(),
                        D3D12_DRED_ENABLEMENT_FORCED_ON,
                    );
                    (s.vtbl().SetPageFaultEnablement)(s.as_raw(), D3D12_DRED_ENABLEMENT_FORCED_ON);
                }
            }
            Err(_) => marlin_diag::warn!("D3D12 DRED unavailable: lost devices report no details"),
        }
    });
}

// --- Validation messages ---

/// The debug layer's message queue, drained into the `marlin_diag` log after submits and
/// presents.
pub(super) struct Validation {
    pub(super) info_queue: Com<ID3D12InfoQueue>,
    /// The info queue is not documented as thread-safe.
    pub(super) lock: AtomicBool,
    pub(super) errors: AtomicU64,
}

impl Validation {
    pub(super) fn flush(&self) {
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
            let (severity, id, text) = unsafe {
                let m = &*message;
                let bytes = core::slice::from_raw_parts(
                    m.pDescription,
                    m.DescriptionByteLength.saturating_sub(1),
                );
                (
                    m.Severity,
                    m.ID,
                    core::str::from_utf8(bytes).unwrap_or("<invalid UTF-8>"),
                )
            };
            match severity {
                D3D12_MESSAGE_SEVERITY_CORRUPTION | D3D12_MESSAGE_SEVERITY_ERROR => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    marlin_diag::error!("D3D12 #{id}: {text}");
                }
                D3D12_MESSAGE_SEVERITY_WARNING => marlin_diag::warn!("D3D12 #{id}: {text}"),
                _ => marlin_diag::debug!("D3D12 #{id}: {text}"),
            }
        }
        // SAFETY: valid interface.
        unsafe { (q.vtbl().ClearStoredMessages)(q.as_raw()) };
        self.lock.store(false, Ordering::Release);
    }
}

pub(super) fn flush(validation: &Option<alloc::sync::Arc<Validation>>) {
    if let Some(v) = validation {
        v.flush();
    }
}

// --- Debug names ---

/// `name` as a NUL-terminated UTF-16 string.
pub(super) fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Names any D3D12 object (`ID3D12Object::SetName`), for debuggers and DRED.
///
/// # Safety
/// `object` is a live D3D12 object (its vtable starts with `ID3D12Object`'s methods).
pub(super) unsafe fn set_name(object: *mut core::ffi::c_void, name: &str) {
    let name = wide(name);
    // SAFETY: the caller guarantees a live D3D12 object; the borrowed reference is released at
    // the end of the block.
    if let Some(o) = unsafe { Com::<ID3D12Object>::from_borrowed(object) } {
        // SAFETY: valid interface and NUL-terminated name.
        unsafe { (o.vtbl().SetName)(o.as_raw(), name.as_ptr()) };
    }
}

// --- Device loss ---

/// Watches one device for removal. Shared by every object that can observe it (queues, fences,
/// swapchains), so whichever notices first logs the crash report, once.
pub(super) struct Lost {
    device: Com<ID3D12Device>,
    reported: AtomicBool,
}

impl Lost {
    pub(super) fn new(device: Com<ID3D12Device>) -> Self {
        Self {
            device,
            reported: AtomicBool::new(false),
        }
    }

    /// The removal reason, or `None` while the device is alive.
    pub(super) fn reason(&self) -> Option<HRESULT> {
        let d = &self.device;
        // SAFETY: valid interface.
        let hr = unsafe { (d.vtbl().GetDeviceRemovedReason)(d.as_raw()) };
        (hr < 0).then_some(hr)
    }

    /// `Err(DeviceLost)` (reporting it once) if the device is gone.
    pub(super) fn check(&self) -> Result<()> {
        match self.reason() {
            None => Ok(()),
            Some(reason) => {
                self.report(reason);
                Err(Error::new(ErrorKind::DeviceLost, reason))
            }
        }
    }

    /// Maps a failed call's `HRESULT`; device-loss codes report the loss.
    pub(super) fn result(&self, hr: HRESULT) -> Result<()> {
        if hr >= 0 {
            return Ok(());
        }
        let error = super::error(hr);
        if error.kind() == ErrorKind::DeviceLost {
            return self.check().and(Err(error));
        }
        Err(error)
    }

    fn report(&self, reason: HRESULT) {
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }
        let mut text = String::new();
        let _ = write!(
            text,
            "D3D12 device lost: {} ({:#010x})",
            reason_name(reason),
            reason as u32
        );
        if let Ok(dred) = self.device.cast::<ID3D12DeviceRemovedExtendedData>() {
            breadcrumbs(&dred, &mut text);
            page_fault(&dred, &mut text);
        } else {
            text.push_str("\n  (no DRED: enable InstanceDesc::crash_diagnostics for details)");
        }
        marlin_diag::error!("{text}");
    }
}

fn reason_name(hr: HRESULT) -> &'static str {
    match hr {
        DXGI_ERROR_DEVICE_HUNG => "the GPU hung (a command took too long, or an infinite loop)",
        DXGI_ERROR_DEVICE_REMOVED => "the device was removed (crash, driver update or unplug)",
        DXGI_ERROR_DEVICE_RESET => "the device was reset (a bad command)",
        DXGI_ERROR_DRIVER_INTERNAL_ERROR => "the driver hit an internal error",
        E_OUTOFMEMORY => "out of memory",
        _ => "unknown reason",
    }
}

/// A NUL-terminated UTF-16 string, or `fallback` for null.
///
/// # Safety
/// `s` is null or NUL-terminated.
unsafe fn from_wide(s: *const u16, fallback: &str) -> String {
    if s.is_null() {
        return String::from(fallback);
    }
    let mut len = 0;
    // SAFETY: NUL-terminated (caller).
    while unsafe { *s.add(len) } != 0 && len < 512 {
        len += 1;
    }
    // SAFETY: `len` units are readable.
    let units = unsafe { core::slice::from_raw_parts(s, len) };
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

const OPS: [&str; 49] = [
    "SetMarker",
    "BeginEvent",
    "EndEvent",
    "DrawInstanced",
    "DrawIndexedInstanced",
    "ExecuteIndirect",
    "Dispatch",
    "CopyBufferRegion",
    "CopyTextureRegion",
    "CopyResource",
    "CopyTiles",
    "ResolveSubresource",
    "ClearRenderTargetView",
    "ClearUnorderedAccessView",
    "ClearDepthStencilView",
    "ResourceBarrier",
    "ExecuteBundle",
    "Present",
    "ResolveQueryData",
    "BeginSubmission",
    "EndSubmission",
    "DecodeFrame",
    "ProcessFrames",
    "AtomicCopyBufferUint",
    "AtomicCopyBufferUint64",
    "ResolveSubresourceRegion",
    "WriteBufferImmediate",
    "DecodeFrame1",
    "SetProtectedResourceSession",
    "DecodeFrame2",
    "ProcessFrames1",
    "BuildRaytracingAccelerationStructure",
    "EmitRaytracingAccelerationStructurePostbuildInfo",
    "CopyRaytracingAccelerationStructure",
    "DispatchRays",
    "InitializeMetaCommand",
    "ExecuteMetaCommand",
    "EstimateMotion",
    "ResolveMotionVectorHeap",
    "SetPipelineState1",
    "InitializeExtensionCommand",
    "ExecuteExtensionCommand",
    "DispatchMesh",
    "EncodeFrame",
    "ResolveEncoderOutputMetadata",
    "Barrier",
    "BeginCommandList",
    "DispatchGraph",
    "SetProgram",
];

fn op_name(op: i32) -> &'static str {
    OPS.get(op as usize).copied().unwrap_or("?")
}

/// Appends the command lists that had not finished, with the operations around where each
/// stopped.
fn breadcrumbs(dred: &Com<ID3D12DeviceRemovedExtendedData>, text: &mut String) {
    let mut output = D3D12_DRED_AUTO_BREADCRUMBS_OUTPUT {
        pHeadAutoBreadcrumbNode: ptr::null(),
    };
    // SAFETY: valid interface and out pointer.
    if unsafe { (dred.vtbl().GetAutoBreadcrumbsOutput)(dred.as_raw(), &mut output) } < 0 {
        return;
    }
    let mut node = output.pHeadAutoBreadcrumbNode;
    let mut incomplete = 0;
    // SAFETY: DRED's list is valid while the device lives; each node's arrays hold
    // `BreadcrumbCount` operations and its strings are NUL-terminated.
    while let Some(n) = unsafe { node.as_ref() } {
        node = n.pNext;
        let count = n.BreadcrumbCount;
        let done = match n.pLastBreadcrumbValue.is_null() {
            true => 0,
            // SAFETY: as above.
            false => unsafe { *n.pLastBreadcrumbValue },
        };
        if done >= count {
            continue;
        }
        incomplete += 1;
        // SAFETY: as above.
        let (queue, list) = unsafe {
            (
                from_wide(n.pCommandQueueDebugNameW, "unnamed queue"),
                from_wide(n.pCommandListDebugNameW, "unnamed list"),
            )
        };
        let _ = write!(
            text,
            "\n  {queue} / {list}: {done} of {count} operations finished"
        );
        let first = done.saturating_sub(3);
        let last = (done + 3).min(count);
        for i in first..last {
            // SAFETY: `i < count`.
            let op = unsafe { *n.pCommandHistory.add(i as usize) };
            let mark = if i == done { "  <- in progress" } else { "" };
            let _ = write!(text, "\n    [{i}] {}{mark}", op_name(op));
        }
    }
    if incomplete == 0 {
        text.push_str("\n  every submitted command list had finished");
    }
}

/// Appends the faulting GPU address and the allocations at it, if there was a page fault.
fn page_fault(dred: &Com<ID3D12DeviceRemovedExtendedData>, text: &mut String) {
    let mut output = D3D12_DRED_PAGE_FAULT_OUTPUT {
        PageFaultVA: 0,
        pHeadExistingAllocationNode: ptr::null(),
        pHeadRecentFreedAllocationNode: ptr::null(),
    };
    // SAFETY: valid interface and out pointer.
    if unsafe { (dred.vtbl().GetPageFaultAllocationOutput)(dred.as_raw(), &mut output) } < 0
        || output.PageFaultVA == 0
    {
        return;
    }
    let _ = write!(
        text,
        "\n  page fault at GPU address {:#x}",
        output.PageFaultVA
    );
    for (head, what) in [
        (output.pHeadExistingAllocationNode, "live"),
        (
            output.pHeadRecentFreedAllocationNode,
            "recently freed (use after free?)",
        ),
    ] {
        let mut node = head;
        // SAFETY: DRED's lists are valid while the device lives; names are NUL-terminated.
        while let Some(n) = unsafe { node.as_ref() } {
            node = n.pNext;
            // SAFETY: as above.
            let name = unsafe { from_wide(n.ObjectNameW, "unnamed") };
            let _ = write!(
                text,
                "\n    {what}: {name} (allocation type {})",
                n.AllocationType
            );
        }
    }
}
