//! The loader library, `VkInstance`, validation, and adapter enumeration.

use core::ffi::{CStr, c_char};
use core::ptr;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use marlin_pal::ext::dylib::{Dylib, Library};

use super::diag::Validation;
use super::ffi::*;
use super::{check, unsupported};
use crate::{AdapterInfo, AdapterKind, InstanceDesc, Result};

#[cfg(marlin_platform = "windows")]
const LOADER: &str = "vulkan-1.dll";
#[cfg(marlin_platform = "linux")]
const LOADER: &str = "libvulkan.so.1";

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Instance extensions enabled when present. Surfaces belong to the swapchain; nothing else
/// needs them, so a machine without a display still gets an instance.
#[derive(Clone, Copy, Default)]
pub(crate) struct InstanceExtensions {
    pub(crate) debug_utils: bool,
    pub(crate) layer_settings: bool,
    pub(crate) surface: bool,
    pub(crate) win32_surface: bool,
    pub(crate) wayland_surface: bool,
    pub(crate) xlib_surface: bool,
    pub(crate) xcb_surface: bool,
    pub(crate) get_surface_capabilities2: bool,
    pub(crate) surface_maintenance1: bool,
    pub(crate) swapchain_colorspace: bool,
}

impl InstanceExtensions {
    /// Each extension with the flag that records it.
    fn table(&mut self) -> [(&'static CStr, &mut bool); 10] {
        [
            (c"VK_EXT_debug_utils", &mut self.debug_utils),
            (c"VK_EXT_layer_settings", &mut self.layer_settings),
            (c"VK_KHR_surface", &mut self.surface),
            (c"VK_KHR_win32_surface", &mut self.win32_surface),
            (c"VK_KHR_wayland_surface", &mut self.wayland_surface),
            (c"VK_KHR_xlib_surface", &mut self.xlib_surface),
            (c"VK_KHR_xcb_surface", &mut self.xcb_surface),
            (
                c"VK_KHR_get_surface_capabilities2",
                &mut self.get_surface_capabilities2,
            ),
            (
                c"VK_KHR_surface_maintenance1",
                &mut self.surface_maintenance1,
            ),
            (
                c"VK_EXT_swapchain_colorspace",
                &mut self.swapchain_colorspace,
            ),
        ]
    }
}

/// What devices and their objects share with the instance; it outlives all of them.
pub(crate) struct InstanceShared {
    pub(crate) raw: VkInstance,
    pub(crate) fns: InstanceFns,
    pub(crate) ext: InstanceExtensions,
    /// `InstanceDesc::crash_diagnostics`: devices record checkpoints and report device faults.
    pub(crate) crash_diagnostics: bool,
    messenger: VkDebugUtilsMessengerEXT,
    /// The messenger's user data; boxed so its address stays put.
    validation: Option<Box<Validation>>,
    /// Unloaded last: every function pointer above points into it.
    _library: Library,
}

// SAFETY: Vulkan instances and their functions may be used from any thread; the messenger is only
// destroyed in `drop`.
unsafe impl Send for InstanceShared {}
// SAFETY: as above; instance-level calls the backend makes need no external synchronization.
unsafe impl Sync for InstanceShared {}

impl InstanceShared {
    /// Validation errors the messenger has counted (0 without validation).
    pub(crate) fn validation_errors(&self) -> u64 {
        self.validation.as_ref().map_or(0, |v| v.errors())
    }
}

impl Drop for InstanceShared {
    fn drop(&mut self) {
        // SAFETY: every device made from the instance is gone (they hold an `Arc` to it).
        unsafe {
            if self.messenger != 0
                && let Some(destroy) = self.fns.vkDestroyDebugUtilsMessengerEXT
            {
                destroy(self.raw, self.messenger, ptr::null());
            }
            (self.fns.vkDestroyInstance)(self.raw, ptr::null());
        }
    }
}

/// One physical device, in enumeration order.
pub(crate) struct Adapter {
    pub(crate) physical: VkPhysicalDevice,
    pub(crate) info: AdapterInfo,
    pub(crate) api_version: u32,
}

// SAFETY: physical device handles may be used from any thread.
unsafe impl Send for Adapter {}
// SAFETY: as above; queries on them need no synchronization.
unsafe impl Sync for Adapter {}

pub(crate) struct Instance {
    pub(crate) shared: Arc<InstanceShared>,
    /// Highest performance first: discrete, integrated, virtual, other, CPU.
    pub(crate) adapters: Vec<Adapter>,
}

fn library() -> Result<(Library, PFN_vkGetInstanceProcAddr)> {
    let library = Dylib::get()
        .ok_or_else(unsupported)?
        .open(LOADER)
        .map_err(|_| unsupported())?;
    // SAFETY: the loader exports vkGetInstanceProcAddr with this signature.
    let get = unsafe { library.symbol::<PFN_vkGetInstanceProcAddr>(c"vkGetInstanceProcAddr") }
        .ok_or_else(unsupported)?;
    Ok((library, get))
}

/// Enumerates with the two-call idiom.
///
/// # Safety
/// `f` writes at most `*count` items when given a buffer.
pub(crate) unsafe fn enumerate<T: Default + Clone>(
    f: impl Fn(*mut u32, *mut T) -> VkResult,
) -> Vec<T> {
    loop {
        let mut count = 0;
        if f(&mut count, ptr::null_mut()) < 0 {
            return Vec::new();
        }
        let mut items = vec![T::default(); count as usize];
        match f(&mut count, items.as_mut_ptr()) {
            VK_INCOMPLETE => continue,
            r if r < 0 => return Vec::new(),
            _ => {
                items.truncate(count as usize);
                return items;
            }
        }
    }
}

impl Instance {
    pub(crate) fn new(desc: &InstanceDesc) -> Result<Self> {
        let (library, get) = library()?;
        // SAFETY: global commands are fetched with a null instance.
        let entry = unsafe { EntryFns::load(&|name| get(ptr::null_mut(), name.as_ptr())) }
            .map_err(|_| unsupported())?;

        let mut version = 0;
        // SAFETY: valid out pointer.
        check(unsafe { (entry.vkEnumerateInstanceVersion)(&mut version) })?;
        if version < API_VERSION_1_4 {
            return Err(unsupported());
        }

        // SAFETY: the two-call idiom with valid pointers.
        let layers = unsafe {
            enumerate::<VkLayerProperties>(|n, p| (entry.vkEnumerateInstanceLayerProperties)(n, p))
        };
        let validation_layer = desc.validation
            && layers
                .iter()
                .any(|l| name_of(&l.layerName) == VALIDATION_LAYER);
        if desc.validation && !validation_layer {
            marlin_diag::warn!(
                "Vulkan validation requested but VK_LAYER_KHRONOS_validation is not installed"
            );
        }

        // Extensions of the loader and implicit layers, plus the validation layer's own.
        // SAFETY: the two-call idiom with valid pointers.
        let mut available = unsafe {
            enumerate::<VkExtensionProperties>(|n, p| {
                (entry.vkEnumerateInstanceExtensionProperties)(ptr::null(), n, p)
            })
        };
        if validation_layer {
            // SAFETY: as above.
            available.extend(unsafe {
                enumerate::<VkExtensionProperties>(|n, p| {
                    (entry.vkEnumerateInstanceExtensionProperties)(VALIDATION_LAYER.as_ptr(), n, p)
                })
            });
        }
        let mut ext = InstanceExtensions::default();
        let mut names: Vec<*const c_char> = Vec::new();
        for (name, enabled) in ext.table() {
            if available.iter().any(|e| name_of(&e.extensionName) == name) {
                *enabled = true;
                names.push(name.as_ptr());
            }
        }

        let validation = validation_layer.then(|| Box::new(Validation::default()));
        let messenger_info = validation
            .as_ref()
            .filter(|_| ext.debug_utils)
            .map(|v| v.messenger_info());

        let (on, off): (VkBool32, VkBool32) = (VK_TRUE, VK_FALSE);
        let setting = |name: &'static CStr, value: &VkBool32| VkLayerSettingEXT {
            pLayerName: VALIDATION_LAYER.as_ptr(),
            pSettingName: name.as_ptr(),
            r#type: VK_LAYER_SETTING_TYPE_BOOL32_EXT,
            valueCount: 1,
            pValues: (value as *const VkBool32).cast(),
        };
        // Shaders' `Debug.Print` (`NonSemantic.DebugPrintf`, 0005 §4.4) comes back through the
        // messenger rather than stdout; GPU-AV only when asked for.
        let mut settings = alloc::vec![
            setting(c"printf_enable", &on),
            setting(c"printf_to_stdout", &off),
        ];
        let gpu_validation = validation_layer && desc.gpu_validation && ext.layer_settings;
        if gpu_validation {
            settings.push(setting(c"gpuav_enable", &on));
        }
        if validation_layer && desc.gpu_validation && !ext.layer_settings {
            marlin_diag::warn!(
                "Vulkan GPU-assisted validation unavailable (no VK_EXT_layer_settings)"
            );
        }
        let settings_info = VkLayerSettingsCreateInfoEXT {
            pNext: messenger_info.as_ref().map_or(ptr::null(), |m| {
                (m as *const VkDebugUtilsMessengerCreateInfoEXT).cast()
            }),
            settingCount: settings.len() as u32,
            pSettings: settings.as_ptr(),
            ..Default::default()
        };
        let next: *const core::ffi::c_void = if validation_layer && ext.layer_settings {
            (&raw const settings_info).cast()
        } else {
            settings_info.pNext
        };

        let app = VkApplicationInfo {
            pEngineName: c"Marlin".as_ptr(),
            apiVersion: API_VERSION_1_4,
            ..Default::default()
        };
        let layer_names = [VALIDATION_LAYER.as_ptr()];
        let info = VkInstanceCreateInfo {
            pNext: next,
            pApplicationInfo: &app,
            enabledLayerCount: validation_layer as u32,
            ppEnabledLayerNames: layer_names.as_ptr(),
            enabledExtensionCount: names.len() as u32,
            ppEnabledExtensionNames: names.as_ptr(),
            ..Default::default()
        };
        let mut raw = ptr::null_mut();
        // SAFETY: valid create info chain and out pointer.
        check(unsafe { (entry.vkCreateInstance)(&info, ptr::null(), &mut raw) })?;

        // SAFETY: instance commands are fetched through the new instance.
        let fns = match unsafe { InstanceFns::load(&|name| get(raw, name.as_ptr())) } {
            Ok(fns) => fns,
            Err(missing) => {
                marlin_diag::warn!("Vulkan instance lacks {missing}");
                // SAFETY: a live instance and a NUL-terminated name.
                if let Some(destroy) = unsafe { get(raw, c"vkDestroyInstance".as_ptr()) } {
                    // SAFETY: vkDestroyInstance has this signature; the instance was just
                    // created and nothing uses it.
                    unsafe {
                        let destroy: PFN_vkDestroyInstance = core::mem::transmute(destroy);
                        destroy(raw, ptr::null());
                    }
                }
                return Err(unsupported());
            }
        };

        let mut messenger = 0;
        if let (Some(info), Some(create)) = (&messenger_info, fns.vkCreateDebugUtilsMessengerEXT) {
            // SAFETY: valid create info (its user data outlives the messenger) and out pointer.
            check(unsafe { create(raw, info, ptr::null(), &mut messenger) })?;
        }

        let shared = Arc::new(InstanceShared {
            raw,
            fns,
            ext,
            crash_diagnostics: desc.crash_diagnostics,
            messenger,
            validation,
            _library: library,
        });
        let adapters = enumerate_adapters(&shared);
        Ok(Self { shared, adapters })
    }

    pub(crate) fn adapter(&self, index: u32) -> Option<AdapterInfo> {
        self.adapters.get(index as usize).map(|a| a.info)
    }
}

fn type_rank(device_type: VkPhysicalDeviceType) -> u32 {
    match device_type {
        VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => 0,
        VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => 1,
        VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => 2,
        VK_PHYSICAL_DEVICE_TYPE_CPU => 4,
        _ => 3,
    }
}

fn enumerate_adapters(instance: &InstanceShared) -> Vec<Adapter> {
    // SAFETY: the two-call idiom with valid pointers.
    let physicals = unsafe {
        enumerate::<usize>(|n, p| {
            (instance.fns.vkEnumeratePhysicalDevices)(instance.raw, n, p.cast())
        })
    };
    let mut adapters: Vec<(u32, Adapter)> = physicals
        .into_iter()
        .map(|p| {
            let physical = p as VkPhysicalDevice;
            let mut id = VkPhysicalDeviceIDProperties::default();
            let mut props = VkPhysicalDeviceProperties2 {
                pNext: (&raw mut id).cast(),
                ..Default::default()
            };
            let mut memory = VkPhysicalDeviceMemoryProperties2::default();
            // SAFETY: a physical device of this instance and valid output chains.
            unsafe {
                (instance.fns.vkGetPhysicalDeviceProperties2)(physical, &mut props);
                (instance.fns.vkGetPhysicalDeviceMemoryProperties2)(physical, &mut memory);
            }
            let p = &props.properties;
            let kind = match p.deviceType {
                VK_PHYSICAL_DEVICE_TYPE_CPU => AdapterKind::Software,
                _ => AdapterKind::Hardware,
            };
            let name = name_of(&p.deviceName).to_str().unwrap_or("");
            let mut info = AdapterInfo::new(0, kind, name);
            info.vendor_id = p.vendorID;
            info.device_id = p.deviceID;
            let m = &memory.memoryProperties;
            for heap in &m.memoryHeaps[..m.memoryHeapCount as usize] {
                if heap.flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT != 0 {
                    info.dedicated_video_memory += heap.size;
                } else {
                    info.shared_system_memory += heap.size;
                }
            }
            if id.deviceLUIDValid != 0 {
                info.luid = u64::from_le_bytes(id.deviceLUID);
            }
            (
                type_rank(p.deviceType),
                Adapter {
                    physical,
                    info,
                    api_version: p.apiVersion,
                },
            )
        })
        .collect();
    adapters.sort_by_key(|(rank, _)| *rank);
    adapters
        .into_iter()
        .enumerate()
        .map(|(i, (_, mut a))| {
            a.info.index = i as u32;
            a
        })
        .collect()
}
