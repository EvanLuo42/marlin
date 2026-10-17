//! Diagnostics: the validation layer's messages, device loss, debug names and native handles.
//! Crash reports (device fault, breadcrumbs) and command-list events are the query/diagnostics
//! area's (0016 §8).

use core::ffi::{CStr, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::vec::Vec;

use marlin_sync::Once;

use super::command::CommandList;
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
        });
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

pub(crate) fn begin_event(list: &mut CommandList, name: &str) {
    let _ = (list, name, ptr::null::<u8>());
    todo!("vulkan: begin_event")
}

pub(crate) fn end_event(list: &mut CommandList) {
    let _ = list;
    todo!("vulkan: end_event")
}

pub(crate) fn set_marker(list: &mut CommandList, name: &str) {
    let _ = (list, name);
    todo!("vulkan: set_marker")
}
