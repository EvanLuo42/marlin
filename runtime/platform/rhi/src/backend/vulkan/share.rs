//! Sharing across devices and processes (external memory and semaphores: Win32 handles on
//! Windows, file descriptors on Linux), and wrapping native objects.
//!
//! Handles are opaque (`OPAQUE_WIN32` NT handles, `OPAQUE_FD`): they open on the same physical
//! device and driver, in this process or another. Committed resources with `SHARED` usage get
//! dedicated, exportable memory ([`External::export`] at creation); importing creates the same
//! resource from its description and allocates its dedicated memory from the handle, which stays
//! owned by the [`SharedHandle`].

use core::ffi::c_void;

use super::Backend;
use super::device::{Device, Shared};
use super::ffi::*;
use super::memory::Memory;
use super::queue::Fence;
use super::resource::{self, Buffer, Texture};
use super::{invalid, unsupported};
use crate::contract::Object;
use crate::{BufferDesc, MemoryLocation, Result, TextureDesc};

#[cfg(marlin_platform = "windows")]
const MEMORY_HANDLE: VkExternalMemoryHandleTypeFlags =
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
#[cfg(not(marlin_platform = "windows"))]
const MEMORY_HANDLE: VkExternalMemoryHandleTypeFlags = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;

#[cfg(marlin_platform = "windows")]
const SEMAPHORE_HANDLE: VkExternalSemaphoreHandleTypeFlags =
    VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_WIN32_BIT;
#[cfg(not(marlin_platform = "windows"))]
const SEMAPHORE_HANDLE: VkExternalSemaphoreHandleTypeFlags =
    VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;

/// The device can export and import memory as OS handles.
fn memory_handles(shared: &Shared) -> bool {
    match cfg!(marlin_platform = "windows") {
        true => shared.ext.external_memory_win32,
        false => shared.ext.external_memory_fd,
    }
}

/// The device can export and import semaphores as OS handles.
fn semaphore_handles(shared: &Shared) -> bool {
    match cfg!(marlin_platform = "windows") {
        true => shared.ext.external_semaphore_win32,
        false => shared.ext.external_semaphore_fd,
    }
}

#[cfg(marlin_platform = "linux")]
mod fd {
    unsafe extern "C" {
        pub(super) fn dup(fd: i32) -> i32;
        pub(super) fn close(fd: i32) -> i32;
    }
}

/// An OS handle to exported memory or a semaphore: an NT handle, or a file descriptor stored in
/// the pointer.
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

    /// The handle as Vulkan imports it. File descriptor imports take ownership, so this is a
    /// duplicate the import consumes (closed by [`release_import`] if the import fails).
    #[cfg(marlin_platform = "linux")]
    fn for_import(&self) -> Result<i32> {
        // SAFETY: duplicating a descriptor this owns.
        let fd = unsafe { fd::dup(self.raw as usize as i32) };
        if fd < 0 {
            return Err(invalid());
        }
        Ok(fd)
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        #[cfg(marlin_platform = "windows")]
        // SAFETY: an NT handle this owns.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.raw);
        }
        #[cfg(marlin_platform = "linux")]
        // SAFETY: a file descriptor this owns.
        unsafe {
            fd::close(self.raw as usize as i32);
        }
    }
}

/// The external-memory structures a resource chains in when its memory is exported or imported:
/// [`buffer_info`](Self::buffer_info) or [`image_info`](Self::image_info) at the end of the
/// resource's create info, [`memory_info`](Self::memory_info) as `Memory::allocate`'s `next`.
/// The memory must be a dedicated allocation of a committed resource.
pub(crate) struct External {
    buffer: VkExternalMemoryBufferCreateInfo,
    image: VkExternalMemoryImageCreateInfo,
    export: VkExportMemoryAllocateInfo,
    #[cfg(marlin_platform = "windows")]
    import: VkImportMemoryWin32HandleInfoKHR,
    #[cfg(not(marlin_platform = "windows"))]
    import: VkImportMemoryFdInfoKHR,
    importing: bool,
}

impl External {
    fn new() -> Self {
        Self {
            buffer: VkExternalMemoryBufferCreateInfo {
                handleTypes: MEMORY_HANDLE,
                ..Default::default()
            },
            image: VkExternalMemoryImageCreateInfo {
                handleTypes: MEMORY_HANDLE,
                ..Default::default()
            },
            export: VkExportMemoryAllocateInfo {
                handleTypes: MEMORY_HANDLE,
                ..Default::default()
            },
            import: Default::default(),
            importing: false,
        }
    }

    /// For a resource being created: exportable memory if `shared` (its `SHARED` usage) and the
    /// device can export; otherwise `None`, and exporting it later fails.
    pub(crate) fn export(device: &Shared, shared: bool) -> Option<Self> {
        (shared && memory_handles(device)).then(Self::new)
    }

    /// Memory imported from `handle`.
    fn import(device: &Shared, handle: &SharedHandle) -> Result<Self> {
        if !memory_handles(device) {
            return Err(unsupported());
        }
        let mut external = Self::new();
        external.importing = true;
        external.import.handleType = MEMORY_HANDLE as VkExternalMemoryHandleTypeFlagBits;
        #[cfg(marlin_platform = "windows")]
        {
            external.import.handle = handle.raw;
        }
        #[cfg(not(marlin_platform = "windows"))]
        {
            external.import.fd = handle.for_import()?;
        }
        Ok(external)
    }

    /// The import did not happen: frees what it would have consumed.
    fn release_import(&self) {
        #[cfg(marlin_platform = "linux")]
        // SAFETY: the duplicate `import` made, which no allocation took.
        unsafe {
            fd::close(self.import.fd);
        }
    }

    /// A `VkExternalMemoryBufferCreateInfo` for the buffer's create info chain.
    pub(crate) fn buffer_info(&self) -> *const c_void {
        (&raw const self.buffer).cast()
    }

    /// A `VkExternalMemoryImageCreateInfo` for the image's create info chain.
    pub(crate) fn image_info(&self) -> *const c_void {
        (&raw const self.image).cast()
    }

    /// The export or import structure for the allocation's chain.
    pub(crate) fn memory_info(&self) -> *const c_void {
        match self.importing {
            true => (&raw const self.import).cast(),
            false => (&raw const self.export).cast(),
        }
    }
}

pub(crate) fn export(device: &Device, object: Object<'_, Backend>) -> Result<SharedHandle> {
    let s = &device.shared;
    s.lost.check()?;
    match object {
        Object::Buffer(b) => export_memory(s, b.memory()),
        Object::Texture(t) => export_memory(s, t.memory()),
        Object::Fence(f) => export_semaphore(s, f),
        _ => Err(invalid()),
    }
}

fn export_memory(s: &Shared, memory: Option<&Memory>) -> Result<SharedHandle> {
    // Placed, sparse, imported and wrapped resources have no exportable memory of their own.
    let memory = memory.filter(|m| m.exportable).ok_or_else(unsupported)?;
    #[cfg(marlin_platform = "windows")]
    {
        let get = s.fns.vkGetMemoryWin32HandleKHR.ok_or_else(unsupported)?;
        let info = VkMemoryGetWin32HandleInfoKHR {
            memory: memory.raw,
            handleType: MEMORY_HANDLE as VkExternalMemoryHandleTypeFlagBits,
            ..Default::default()
        };
        let mut raw = core::ptr::null_mut();
        // SAFETY: memory allocated with this handle type exportable; a valid out pointer.
        let r = unsafe { get(s.raw, &info, &mut raw) };
        s.lost.result(r)?;
        Ok(SharedHandle { raw })
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        let get = s.fns.vkGetMemoryFdKHR.ok_or_else(unsupported)?;
        let info = VkMemoryGetFdInfoKHR {
            memory: memory.raw,
            handleType: MEMORY_HANDLE as VkExternalMemoryHandleTypeFlagBits,
            ..Default::default()
        };
        let mut fd = -1;
        // SAFETY: as above.
        s.lost.result(unsafe { get(s.raw, &info, &mut fd) })?;
        Ok(SharedHandle {
            raw: fd as usize as *mut c_void,
        })
    }
}

fn export_semaphore(s: &Shared, fence: &Fence) -> Result<SharedHandle> {
    // Only `create_shared_fence` makes exportable semaphores.
    if !fence.exportable() || !semaphore_handles(s) {
        return Err(invalid());
    }
    #[cfg(marlin_platform = "windows")]
    {
        let get = s.fns.vkGetSemaphoreWin32HandleKHR.ok_or_else(unsupported)?;
        let info = VkSemaphoreGetWin32HandleInfoKHR {
            semaphore: fence.raw(),
            handleType: SEMAPHORE_HANDLE as VkExternalSemaphoreHandleTypeFlagBits,
            ..Default::default()
        };
        let mut raw = core::ptr::null_mut();
        // SAFETY: a semaphore created exportable as this handle type; a valid out pointer.
        s.lost.result(unsafe { get(s.raw, &info, &mut raw) })?;
        Ok(SharedHandle { raw })
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        let get = s.fns.vkGetSemaphoreFdKHR.ok_or_else(unsupported)?;
        let info = VkSemaphoreGetFdInfoKHR {
            semaphore: fence.raw(),
            handleType: SEMAPHORE_HANDLE as VkExternalSemaphoreHandleTypeFlagBits,
            ..Default::default()
        };
        let mut fd = -1;
        // SAFETY: as above.
        s.lost.result(unsafe { get(s.raw, &info, &mut fd) })?;
        Ok(SharedHandle {
            raw: fd as usize as *mut c_void,
        })
    }
}

/// # Safety
/// As `contract::Device::import_texture`.
pub(crate) unsafe fn import_texture(
    device: &Device,
    handle: &SharedHandle,
    desc: &TextureDesc,
) -> Result<Texture> {
    device.lost.check()?;
    let external = External::import(&device.shared, handle)?;
    let texture =
        resource::import_texture_with(device, desc, external.image_info(), external.memory_info());
    if texture.is_err() {
        external.release_import();
    }
    texture
}

/// # Safety
/// As `contract::Device::import_buffer`.
pub(crate) unsafe fn import_buffer(
    device: &Device,
    handle: &SharedHandle,
    desc: &BufferDesc,
) -> Result<Buffer> {
    device.lost.check()?;
    let external = External::import(&device.shared, handle)?;
    let buffer = resource::create_buffer_with(device, desc, Some(&external));
    if buffer.is_err() {
        external.release_import();
    }
    buffer
}

/// # Safety
/// As `contract::Device::import_fence`.
pub(crate) unsafe fn import_fence(device: &Device, handle: &SharedHandle) -> Result<Fence> {
    let s = &device.shared;
    s.lost.check()?;
    if !semaphore_handles(s) {
        return Err(unsupported());
    }
    // The payload replaces the new semaphore's; both are timelines.
    let fence = Fence::create(s, 0, false)?;
    #[cfg(marlin_platform = "windows")]
    {
        let import = s
            .fns
            .vkImportSemaphoreWin32HandleKHR
            .ok_or_else(unsupported)?;
        let info = VkImportSemaphoreWin32HandleInfoKHR {
            semaphore: fence.raw(),
            handleType: SEMAPHORE_HANDLE as VkExternalSemaphoreHandleTypeFlagBits,
            handle: handle.raw,
            ..Default::default()
        };
        // SAFETY: a new timeline semaphore nothing uses; the caller guarantees the handle names
        // a fence (an exported timeline semaphore). NT handle imports do not take the handle.
        s.lost.result(unsafe { import(s.raw, &info) })?;
    }
    #[cfg(not(marlin_platform = "windows"))]
    {
        let import = s.fns.vkImportSemaphoreFdKHR.ok_or_else(unsupported)?;
        let fd = handle.for_import()?;
        let info = VkImportSemaphoreFdInfoKHR {
            semaphore: fence.raw(),
            handleType: SEMAPHORE_HANDLE as VkExternalSemaphoreHandleTypeFlagBits,
            fd,
            ..Default::default()
        };
        // SAFETY: as above; a successful import takes the duplicate.
        if let Err(e) = s.lost.result(unsafe { import(s.raw, &info) }) {
            // SAFETY: the duplicate the failed import did not take.
            unsafe { fd::close(fd) };
            return Err(e);
        }
    }
    Ok(fence)
}

/// # Safety
/// As `contract::Device::texture_from_native`: `raw` is a `VkImage` of this device, bound to
/// memory, in `GENERAL` layout, which `desc` describes. The wrapper does not own it.
pub(crate) unsafe fn texture_from_native(
    device: &Device,
    raw: *mut c_void,
    desc: &TextureDesc,
) -> Result<Texture> {
    device.lost.check()?;
    Ok(Texture::from_image(
        &device.shared,
        raw as usize as VkImage,
        desc,
        false,
    ))
}

/// # Safety
/// As `contract::Device::buffer_from_native`: `raw` is a `VkBuffer` of this device, bound to
/// memory and created with `VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT` (buffer views are
/// device addresses), which `desc` describes. The wrapper does not own it. Vulkan cannot find a
/// buffer's memory, so CPU-visible ones (which would need its mapping) are `Unsupported`.
pub(crate) unsafe fn buffer_from_native(
    device: &Device,
    raw: *mut c_void,
    desc: &BufferDesc,
) -> Result<Buffer> {
    device.lost.check()?;
    if desc.memory != MemoryLocation::GpuOnly {
        return Err(unsupported());
    }
    Ok(Buffer::from_parts(
        &device.shared,
        raw as usize as VkBuffer,
        None,
        None,
        false,
    ))
}
