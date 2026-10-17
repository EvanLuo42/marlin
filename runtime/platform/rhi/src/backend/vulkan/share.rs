//! Sharing across devices and processes (external memory and semaphores: Win32 handles on
//! Windows, file descriptors on Linux), and wrapping native objects.

use core::ffi::c_void;

use super::Backend;
use super::device::Device;
use super::queue::Fence;
use super::resource::{Buffer, Texture};
use crate::contract::Object;
use crate::{BufferDesc, Result, TextureDesc};

/// An OS handle to exported memory or a semaphore.
pub(crate) struct SharedHandle {
    raw: *mut c_void,
}

// SAFETY: OS handles are process-wide.
unsafe impl Send for SharedHandle {}
// SAFETY: as above.
unsafe impl Sync for SharedHandle {}

impl SharedHandle {
    pub(crate) fn as_raw(&self) -> *mut c_void {
        self.raw
    }

    /// # Safety
    /// `raw` is an owned handle of the kind `export` returns.
    pub(crate) unsafe fn from_raw(raw: *mut c_void) -> Self {
        Self { raw }
    }
}

pub(crate) fn export(device: &Device, object: Object<'_, Backend>) -> Result<SharedHandle> {
    let _ = (device, object);
    todo!("vulkan: export")
}

/// # Safety
/// As `contract::Device::import_texture`.
pub(crate) unsafe fn import_texture(
    device: &Device,
    handle: &SharedHandle,
    desc: &TextureDesc,
) -> Result<Texture> {
    let _ = (device, handle, desc);
    todo!("vulkan: import_texture")
}

/// # Safety
/// As `contract::Device::import_buffer`.
pub(crate) unsafe fn import_buffer(
    device: &Device,
    handle: &SharedHandle,
    desc: &BufferDesc,
) -> Result<Buffer> {
    let _ = (device, handle, desc);
    todo!("vulkan: import_buffer")
}

/// # Safety
/// As `contract::Device::import_fence`.
pub(crate) unsafe fn import_fence(device: &Device, handle: &SharedHandle) -> Result<Fence> {
    let _ = (device, handle);
    todo!("vulkan: import_fence")
}

/// # Safety
/// As `contract::Device::texture_from_native`.
pub(crate) unsafe fn texture_from_native(
    device: &Device,
    raw: *mut c_void,
    desc: &TextureDesc,
) -> Result<Texture> {
    let _ = (device, raw, desc);
    todo!("vulkan: texture_from_native")
}

/// # Safety
/// As `contract::Device::buffer_from_native`.
pub(crate) unsafe fn buffer_from_native(
    device: &Device,
    raw: *mut c_void,
    desc: &BufferDesc,
) -> Result<Buffer> {
    let _ = (device, raw, desc);
    todo!("vulkan: buffer_from_native")
}
