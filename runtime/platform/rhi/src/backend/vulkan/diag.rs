//! Diagnostics: the validation layer's messages, device loss, debug names and native handles.
//! Crash reports (device fault, breadcrumbs) and command-list events are the query/diagnostics
//! area's (0016 §8).

use core::ffi::{CStr, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use marlin_sync::{Mutex, Once, OnceLock};

use super::command::CommandList;
use super::device::Shared;
use super::ffi::*;
use super::{Backend, error, handle_ptr};
use crate::contract::Object;
use crate::{Error, ErrorKind, Result};

// --- Validation ---

/// Counts the validation layer's errors; the debug messenger's user data.
#[derive(Default)]
pub(crate) struct Validation {
    errors: AtomicU64,
}

impl Validation {
    pub(crate) fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// A messenger that logs warnings and errors and counts errors into `self`, which must
    /// outlive it.
    pub(crate) fn messenger_info(&self) -> VkDebugUtilsMessengerCreateInfoEXT {
        VkDebugUtilsMessengerCreateInfoEXT {
            messageSeverity: VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
            messageType: VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT
                | VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT,
            pfnUserCallback: Some(messenger),
            pUserData: (self as *const Self).cast_mut().cast(),
            ..Default::default()
        }
    }
}

unsafe extern "system" fn messenger(
    severity: VkDebugUtilsMessageSeverityFlagsEXT,
    types: VkDebugUtilsMessageTypeFlagsEXT,
    data: *const VkDebugUtilsMessengerCallbackDataEXT,
    user: *mut c_void,
) -> VkBool32 {
    // SAFETY: the layer passes valid callback data for the duration of the call.
    let data = unsafe { &*data };
    let text = |p: *const core::ffi::c_char| {
        if p.is_null() {
            ""
        } else {
            // SAFETY: the layer's strings are NUL-terminated.
            unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
        }
    };
    let message = text(data.pMessage);
    if severity & VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT != 0 {
        // SAFETY: the user data is the `Validation` that outlives the messenger.
        let validation = unsafe { &*user.cast::<Validation>() };
        validation.errors.fetch_add(1, Ordering::Relaxed);
        marlin_diag::error!("Vulkan: {message}");
    } else if types & VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT == 0 {
        // Performance hints, and the loader's notes about implicit layers (overlays, switchable
        // graphics) that the application does not control.
        marlin_diag::debug!("Vulkan: {message}");
    } else {
        marlin_diag::warn!("Vulkan: {message}");
    }
    VK_FALSE
}

// --- Device loss ---

/// Whether the device is lost, set by the first call that sees `VK_ERROR_DEVICE_LOST` (or by
/// [`force`](Self::force)); reports it once.
#[derive(Default)]
pub(crate) struct Lost {
    lost: AtomicBool,
    report: Once,
    /// What the report draws on (crash report section).
    crash: Crash,
}

impl Lost {
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<()> {
        match self.is_lost() {
            true => Err(Error::from_kind(ErrorKind::DeviceLost)),
            false => Ok(()),
        }
    }

    /// Records a lost device if `result` says so (logging the report the first time), then maps
    /// failures to errors.
    pub(crate) fn result(&self, result: VkResult) -> Result<()> {
        if result == VK_ERROR_DEVICE_LOST {
            self.mark(None);
        }
        if result < 0 {
            Err(error(result))
        } else {
            Ok(())
        }
    }

    /// Marks the device lost; `report` writes what is known about the loss (the crash report).
    pub(crate) fn mark(&self, report: Option<&dyn Fn()>) {
        self.lost.store(true, Ordering::Release);
        self.report.call_once(|| {
            marlin_diag::error!("Vulkan device lost");
            if let Some(report) = report {
                report();
            }
            self.crash.report();
        });
    }

    /// Lets the crash report reach `shared`, the device this belongs to; called once, when the
    /// device is created.
    pub(crate) fn attach(&self, shared: &Arc<Shared>) {
        if shared.instance.crash_diagnostics {
            let _ = self.crash.device.set(Arc::downgrade(shared));
        }
    }

    /// Vulkan cannot remove a device on purpose; the RHI reports it lost from now on, which is
    /// what the recovery path sees.
    pub(crate) fn force(&self) {
        self.lost.store(true, Ordering::Release);
    }
}

// --- Names and native handles ---

/// The object's type and handle as debug utils and `as_raw` name it.
fn object_handle(object: &Object<'_, Backend>) -> (VkObjectType, u64) {
    match *object {
        Object::Instance(i) => (VK_OBJECT_TYPE_INSTANCE, i.shared.raw as u64),
        Object::Device(d) => (VK_OBJECT_TYPE_DEVICE, d.shared.raw as u64),
        Object::Queue(q) => (VK_OBJECT_TYPE_QUEUE, q.raw() as u64),
        Object::Fence(f) => (VK_OBJECT_TYPE_SEMAPHORE, f.raw()),
        Object::CommandPool(p) => (VK_OBJECT_TYPE_COMMAND_POOL, p.raw()),
        Object::CommandList(l) => (VK_OBJECT_TYPE_COMMAND_BUFFER, l.raw() as u64),
        Object::Swapchain(s) => (VK_OBJECT_TYPE_SWAPCHAIN_KHR, s.raw()),
        Object::Buffer(b) => (VK_OBJECT_TYPE_BUFFER, b.raw()),
        Object::Texture(t) => (VK_OBJECT_TYPE_IMAGE, t.image()),
        Object::GraphicsPipeline(p) => (VK_OBJECT_TYPE_PIPELINE, p.raw()),
        Object::ComputePipeline(p) => (VK_OBJECT_TYPE_PIPELINE, p.raw()),
        Object::MeshPipeline(p) => (VK_OBJECT_TYPE_PIPELINE, p.raw()),
        Object::QueryPool(p) => (VK_OBJECT_TYPE_QUERY_POOL, p.raw()),
        Object::Heap(h) => (VK_OBJECT_TYPE_DEVICE_MEMORY, h.raw()),
        Object::AccelerationStructure(a) => (VK_OBJECT_TYPE_ACCELERATION_STRUCTURE_KHR, a.raw()),
    }
}

/// The device an object belongs to, for naming it (`None` for the instance).
fn object_device<'a>(object: &Object<'a, Backend>) -> Option<&'a super::device::Shared> {
    Some(match *object {
        Object::Instance(_) => return None,
        Object::Device(d) => &d.shared,
        Object::Queue(q) => q.shared(),
        Object::Fence(f) => f.shared(),
        Object::CommandPool(p) => p.shared(),
        Object::CommandList(l) => l.shared(),
        Object::Swapchain(s) => s.shared(),
        Object::Buffer(b) => b.shared(),
        Object::Texture(t) => t.shared(),
        Object::GraphicsPipeline(p) => p.shared(),
        Object::ComputePipeline(p) => p.shared(),
        Object::MeshPipeline(p) => p.shared(),
        Object::QueryPool(p) => p.shared(),
        Object::Heap(h) => h.shared(),
        Object::AccelerationStructure(a) => a.shared(),
    })
}

/// A NUL-terminated copy of `name`, interior NULs dropped.
pub(crate) fn c_name(name: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = name.bytes().filter(|&b| b != 0).collect();
    bytes.push(0);
    bytes
}

pub(crate) fn set_name(object: Object<'_, Backend>, name: &str) {
    let Some(shared) = object_device(&object) else {
        return;
    };
    let Some(set) = shared.instance.fns.vkSetDebugUtilsObjectNameEXT else {
        return;
    };
    let (ty, handle) = object_handle(&object);
    if handle == 0 {
        return;
    }
    let name = c_name(name);
    let info = VkDebugUtilsObjectNameInfoEXT {
        objectType: ty,
        objectHandle: handle,
        pObjectName: name.as_ptr().cast(),
        ..Default::default()
    };
    // SAFETY: a live object of this device and a valid name.
    unsafe { set(shared.raw, &info) };
}

pub(crate) fn native(object: Object<'_, Backend>) -> *mut c_void {
    match object {
        Object::Instance(i) => i.shared.raw,
        Object::Device(d) => d.shared.raw,
        Object::Queue(q) => q.raw(),
        Object::CommandList(l) => l.raw(),
        other => handle_ptr(object_handle(&other).1),
    }
}

// --- Events (query and diagnostics area) ---

/// A debug utils label of `name`, a NUL-terminated string (debuggers show the region or point).
fn label(name: &[u8]) -> VkDebugUtilsLabelEXT {
    VkDebugUtilsLabelEXT {
        pLabelName: name.as_ptr().cast(),
        ..Default::default()
    }
}

/// Labels need `VK_EXT_debug_utils`; without it events are no-ops. With crash diagnostics,
/// regions and markers also leave checkpoints for the report.
pub(crate) fn begin_event(list: &mut CommandList, name: &str) {
    checkpoint(list, name);
    if let Some(begin) = list.shared().instance.fns.vkCmdBeginDebugUtilsLabelEXT {
        let name = c_name(name);
        // SAFETY: a recording buffer and a NUL-terminated name that outlives the call.
        unsafe { begin(list.raw(), &label(&name)) };
    }
}

pub(crate) fn end_event(list: &mut CommandList) {
    if let Some(end) = list.shared().instance.fns.vkCmdEndDebugUtilsLabelEXT {
        // SAFETY: a recording buffer with a label open (`begin_event` pairs with this).
        unsafe { end(list.raw()) };
    }
}

pub(crate) fn set_marker(list: &mut CommandList, name: &str) {
    checkpoint(list, name);
    if let Some(insert) = list.shared().instance.fns.vkCmdInsertDebugUtilsLabelEXT {
        let name = c_name(name);
        // SAFETY: as in `begin_event`.
        unsafe { insert(list.raw(), &label(&name)) };
    }
}

// --- Crash reports (query and diagnostics area) ---

/// Distinct checkpoint names kept per device; names past it share the "(unnamed)" marker.
const MAX_CHECKPOINT_NAMES: usize = 4096;

/// What a lost device's report draws on, with `InstanceDesc::crash_diagnostics`: the device, to
/// ask it what happened, and the names checkpoints stand for.
#[derive(Default)]
struct Crash {
    /// Set when the device is created with crash diagnostics; weak, since the device owns this.
    device: OnceLock<Weak<Shared>>,
    /// Event and marker names; a checkpoint's marker is its name's index plus one (0 for a name
    /// past the limit).
    names: Mutex<BTreeMap<Box<str>, usize>>,
}

impl Crash {
    /// The checkpoint marker of `name`.
    fn marker(&self, name: &str) -> usize {
        let mut names = self.names.lock();
        if let Some(&marker) = names.get(name) {
            return marker;
        }
        if names.len() >= MAX_CHECKPOINT_NAMES {
            return 0;
        }
        let marker = names.len() + 1;
        names.insert(name.into(), marker);
        marker
    }

    fn name(&self, marker: usize) -> Box<str> {
        let names = self.names.lock();
        names
            .iter()
            .find(|&(_, &m)| m == marker)
            .map_or_else(|| "(unnamed)".into(), |(name, _)| name.clone())
    }

    /// Logs the device fault report and the last checkpoints each queue reached.
    fn report(&self) {
        let Some(shared) = self.device.get().and_then(Weak::upgrade) else {
            return;
        };
        fault(&shared);
        self.checkpoints(&shared);
    }

    /// `VK_NV_device_diagnostic_checkpoints`: per queue, the last checkpoint each pipeline stage
    /// reached.
    fn checkpoints(&self, s: &Shared) {
        let Some(get) = s
            .fns
            .vkGetQueueCheckpointData2NV
            .filter(|_| s.ext.diagnostic_checkpoints)
        else {
            return;
        };
        for &family in &s.families.unique {
            let info = VkDeviceQueueInfo2 {
                queueFamilyIndex: family,
                ..Default::default()
            };
            let mut queue = ptr::null_mut();
            // SAFETY: the device has one queue in each family it uses.
            unsafe { (s.fns.vkGetDeviceQueue2)(s.raw, &info, &mut queue) };
            let mut count = 0;
            // SAFETY: the two-call idiom with valid pointers.
            unsafe { get(queue, &mut count, ptr::null_mut()) };
            let mut data = alloc::vec![VkCheckpointData2NV::default(); count as usize];
            // SAFETY: as above; `data` holds `count` entries.
            unsafe { get(queue, &mut count, data.as_mut_ptr()) };
            for d in &data[..count as usize] {
                marlin_diag::error!(
                    "  queue family {family}: reached \"{}\" at stage {:#x}",
                    self.name(d.pCheckpointMarker as usize),
                    d.stage
                );
            }
        }
    }
}

/// `VK_EXT_device_fault`: the driver's description, faulting addresses and vendor codes.
fn fault(s: &Shared) {
    let Some(get) = s.fns.vkGetDeviceFaultInfoEXT.filter(|_| s.ext.device_fault) else {
        return;
    };
    let mut counts = VkDeviceFaultCountsEXT::default();
    // SAFETY: a valid output structure.
    if unsafe { get(s.raw, &mut counts, ptr::null_mut()) } < 0 {
        return;
    }
    let mut addresses =
        alloc::vec![VkDeviceFaultAddressInfoKHR::default(); counts.addressInfoCount as usize];
    let mut vendor =
        alloc::vec![VkDeviceFaultVendorInfoKHR::default(); counts.vendorInfoCount as usize];
    // The vendor's binary dump is for vendor tools: not asked for.
    counts.vendorBinarySize = 0;
    let mut info = VkDeviceFaultInfoEXT {
        pAddressInfos: addresses.as_mut_ptr(),
        pVendorInfos: vendor.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: arrays of the counted sizes and a valid output structure.
    if unsafe { get(s.raw, &mut counts, &mut info) } < 0 {
        return;
    }
    fn text(chars: &[core::ffi::c_char]) -> &str {
        name_of(chars).to_str().unwrap_or("")
    }
    marlin_diag::error!("Vulkan device fault: {}", text(&info.description));
    for a in &addresses[..counts.addressInfoCount as usize] {
        let kind = match a.addressType {
            VK_DEVICE_FAULT_ADDRESS_TYPE_READ_INVALID_KHR => "invalid read",
            VK_DEVICE_FAULT_ADDRESS_TYPE_WRITE_INVALID_KHR => "invalid write",
            VK_DEVICE_FAULT_ADDRESS_TYPE_EXECUTE_INVALID_KHR => "invalid execute",
            VK_DEVICE_FAULT_ADDRESS_TYPE_INSTRUCTION_POINTER_UNKNOWN_KHR => {
                "instruction pointer (unknown fault)"
            }
            VK_DEVICE_FAULT_ADDRESS_TYPE_INSTRUCTION_POINTER_INVALID_KHR => {
                "instruction pointer (invalid instruction)"
            }
            VK_DEVICE_FAULT_ADDRESS_TYPE_INSTRUCTION_POINTER_FAULT_KHR => {
                "instruction pointer (fault)"
            }
            _ => "address",
        };
        // The address is known to within `addressPrecision` (a power of two) bytes.
        let precision = a.addressPrecision.max(1);
        let base = a.reportedAddress & !(precision - 1);
        marlin_diag::error!(
            "  {kind} at {:#x} (within {base:#x}..{:#x})",
            a.reportedAddress,
            base.saturating_add(precision)
        );
    }
    for v in &vendor[..counts.vendorInfoCount as usize] {
        marlin_diag::error!(
            "  vendor fault {:#x} ({:#x}): {}",
            v.vendorFaultCode,
            v.vendorFaultData,
            text(&v.description)
        );
    }
}

/// Leaves a checkpoint named `name` in the list, with crash diagnostics on a device that takes
/// them.
fn checkpoint(list: &CommandList, name: &str) {
    let s = list.shared();
    let Some(set) = s
        .fns
        .vkCmdSetCheckpointNV
        .filter(|_| s.ext.diagnostic_checkpoints)
    else {
        return;
    };
    if s.lost.crash.device.get().is_none() {
        return;
    }
    let marker = s.lost.crash.marker(name);
    // SAFETY: a recording buffer; the marker is an opaque value the driver hands back.
    unsafe { set(list.raw(), marker as *const c_void) };
}
