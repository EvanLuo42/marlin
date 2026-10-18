//! Vulkan FFI: the generated structures, function types and constants (`generated.rs`, from
//! `vk.spec` by `gen.awk`), the few items written by hand (callbacks, bitfield structures), and
//! the function tables loaded at run time.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

#[rustfmt::skip]
mod generated;

use core::ffi::{CStr, c_char, c_void};

pub(crate) use generated::*;

// --- Written by hand ---

pub(crate) type PFN_vkVoidFunction = Option<unsafe extern "system" fn()>;

/// Never passed: every allocation uses the driver's allocator (null callbacks).
pub(crate) enum VkAllocationCallbacks {}

pub(crate) type PFN_vkDebugUtilsMessengerCallbackEXT = Option<
    unsafe extern "system" fn(
        messageSeverity: VkDebugUtilsMessageSeverityFlagsEXT,
        messageTypes: VkDebugUtilsMessageTypeFlagsEXT,
        pCallbackData: *const VkDebugUtilsMessengerCallbackDataEXT,
        pUserData: *mut c_void,
    ) -> VkBool32,
>;

pub(crate) const fn make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

pub(crate) const API_VERSION_1_4: u32 = make_api_version(0, 1, 4, 0);

/// A fixed-size, NUL-padded name from a Vulkan structure as a `CStr` (empty if unterminated).
pub(crate) fn name_of(chars: &[c_char]) -> &CStr {
    // SAFETY: `c_char` and `u8` have the same layout.
    let bytes = unsafe { core::slice::from_raw_parts(chars.as_ptr().cast::<u8>(), chars.len()) };
    CStr::from_bytes_until_nul(bytes).unwrap_or(c"")
}

// --- Function tables ---

/// A table of Vulkan functions loaded by name: required ones fail the load when missing,
/// optional ones (from extensions that may be absent) are `None`.
macro_rules! fn_table {
    (
        $(#[$meta:meta])* $table:ident {
            $($name:ident: $ty:ty,)*
        }
        optional {
            $($oname:ident: $oty:ty,)*
        }
    ) => {
        $(#[$meta])*
        pub(crate) struct $table {
            $(pub(crate) $name: $ty,)*
            $(pub(crate) $oname: Option<$oty>,)*
        }

        impl $table {
            /// Loads every function through `get`; `Err` names the first required one missing.
            ///
            /// # Safety
            /// `get` returns the named function of the right type, or `None`.
            pub(crate) unsafe fn load(
                get: &dyn Fn(&CStr) -> PFN_vkVoidFunction,
            ) -> Result<Self, &'static str> {
                Ok(Self {
                    $($name: {
                        let name = concat!(stringify!($name), "\0");
                        match get(CStr::from_bytes_with_nul(name.as_bytes()).unwrap()) {
                            // SAFETY: the loader returned this function under its name, so it
                            // has this signature.
                            Some(f) => unsafe {
                                core::mem::transmute::<unsafe extern "system" fn(), $ty>(f)
                            },
                            None => return Err(stringify!($name)),
                        }
                    },)*
                    $($oname: {
                        let name = concat!(stringify!($oname), "\0");
                        get(CStr::from_bytes_with_nul(name.as_bytes()).unwrap()).map(|f| {
                            // SAFETY: as above.
                            unsafe { core::mem::transmute::<unsafe extern "system" fn(), $oty>(f) }
                        })
                    },)*
                })
            }
        }
    };
}

fn_table! {
    /// Global functions, through `vkGetInstanceProcAddr(NULL, ...)`.
    EntryFns {
        vkCreateInstance: PFN_vkCreateInstance,
        vkEnumerateInstanceExtensionProperties: PFN_vkEnumerateInstanceExtensionProperties,
        vkEnumerateInstanceLayerProperties: PFN_vkEnumerateInstanceLayerProperties,
        vkEnumerateInstanceVersion: PFN_vkEnumerateInstanceVersion,
    }
    optional {}
}

fn_table! {
    /// Instance functions, and the instance-level extension functions that dispatch on devices
    /// (debug utils).
    InstanceFns {
        vkDestroyInstance: PFN_vkDestroyInstance,
        vkEnumeratePhysicalDevices: PFN_vkEnumeratePhysicalDevices,
        vkGetPhysicalDeviceProperties2: PFN_vkGetPhysicalDeviceProperties2,
        vkGetPhysicalDeviceFeatures2: PFN_vkGetPhysicalDeviceFeatures2,
        vkGetPhysicalDeviceMemoryProperties2: PFN_vkGetPhysicalDeviceMemoryProperties2,
        vkGetPhysicalDeviceQueueFamilyProperties: PFN_vkGetPhysicalDeviceQueueFamilyProperties,
        vkEnumerateDeviceExtensionProperties: PFN_vkEnumerateDeviceExtensionProperties,
        vkCreateDevice: PFN_vkCreateDevice,
        vkGetDeviceProcAddr: PFN_vkGetDeviceProcAddr,
        // --- resource (formats) ---
        vkGetPhysicalDeviceFormatProperties2: PFN_vkGetPhysicalDeviceFormatProperties2,
        vkGetPhysicalDeviceImageFormatProperties2: PFN_vkGetPhysicalDeviceImageFormatProperties2,
        vkGetPhysicalDeviceSparseImageFormatProperties2:
            PFN_vkGetPhysicalDeviceSparseImageFormatProperties2,
    }
    optional {
        vkGetPhysicalDeviceDescriptorSizeEXT: PFN_vkGetPhysicalDeviceDescriptorSizeEXT,
        vkCreateDebugUtilsMessengerEXT: PFN_vkCreateDebugUtilsMessengerEXT,
        vkDestroyDebugUtilsMessengerEXT: PFN_vkDestroyDebugUtilsMessengerEXT,
        vkSetDebugUtilsObjectNameEXT: PFN_vkSetDebugUtilsObjectNameEXT,
        vkCmdBeginDebugUtilsLabelEXT: PFN_vkCmdBeginDebugUtilsLabelEXT,
        vkCmdEndDebugUtilsLabelEXT: PFN_vkCmdEndDebugUtilsLabelEXT,
        vkCmdInsertDebugUtilsLabelEXT: PFN_vkCmdInsertDebugUtilsLabelEXT,
        // --- swapchain (surfaces) ---
        // --- query (calibrated timestamps) ---
    }
}

fn_table! {
    /// Device functions, through `vkGetDeviceProcAddr`. Optional ones come from optional
    /// extensions; check the extension, not just the pointer.
    DeviceFns {
        vkDestroyDevice: PFN_vkDestroyDevice,
        vkDeviceWaitIdle: PFN_vkDeviceWaitIdle,
        vkGetDeviceQueue2: PFN_vkGetDeviceQueue2,
        vkQueueSubmit2: PFN_vkQueueSubmit2,
        vkQueueWaitIdle: PFN_vkQueueWaitIdle,
        vkCreateSemaphore: PFN_vkCreateSemaphore,
        vkDestroySemaphore: PFN_vkDestroySemaphore,
        vkGetSemaphoreCounterValue: PFN_vkGetSemaphoreCounterValue,
        vkWaitSemaphores: PFN_vkWaitSemaphores,
        vkSignalSemaphore: PFN_vkSignalSemaphore,
        vkCreateFence: PFN_vkCreateFence,
        vkDestroyFence: PFN_vkDestroyFence,
        vkWaitForFences: PFN_vkWaitForFences,
        vkResetFences: PFN_vkResetFences,
        vkAllocateMemory: PFN_vkAllocateMemory,
        vkFreeMemory: PFN_vkFreeMemory,
        vkMapMemory: PFN_vkMapMemory,
        vkUnmapMemory: PFN_vkUnmapMemory,
        vkCreateBuffer: PFN_vkCreateBuffer,
        vkDestroyBuffer: PFN_vkDestroyBuffer,
        vkGetDeviceBufferMemoryRequirements: PFN_vkGetDeviceBufferMemoryRequirements,
        vkGetBufferMemoryRequirements2: PFN_vkGetBufferMemoryRequirements2,
        vkBindBufferMemory2: PFN_vkBindBufferMemory2,
        vkGetBufferDeviceAddress: PFN_vkGetBufferDeviceAddress,
        vkCreateCommandPool: PFN_vkCreateCommandPool,
        vkDestroyCommandPool: PFN_vkDestroyCommandPool,
        vkResetCommandPool: PFN_vkResetCommandPool,
        vkAllocateCommandBuffers: PFN_vkAllocateCommandBuffers,
        vkFreeCommandBuffers: PFN_vkFreeCommandBuffers,
        vkBeginCommandBuffer: PFN_vkBeginCommandBuffer,
        vkEndCommandBuffer: PFN_vkEndCommandBuffer,
        vkCmdPipelineBarrier2: PFN_vkCmdPipelineBarrier2,
        vkCmdCopyBuffer2: PFN_vkCmdCopyBuffer2,
        vkCmdFillBuffer: PFN_vkCmdFillBuffer,
        vkCmdBindPipeline: PFN_vkCmdBindPipeline,
        vkCmdDispatch: PFN_vkCmdDispatch,
        vkCreateComputePipelines: PFN_vkCreateComputePipelines,
        vkDestroyPipeline: PFN_vkDestroyPipeline,
        vkCreatePipelineCache: PFN_vkCreatePipelineCache,
        vkDestroyPipelineCache: PFN_vkDestroyPipelineCache,
        vkGetPipelineCacheData: PFN_vkGetPipelineCacheData,
        vkWriteResourceDescriptorsEXT: PFN_vkWriteResourceDescriptorsEXT,
        vkWriteSamplerDescriptorsEXT: PFN_vkWriteSamplerDescriptorsEXT,
        vkCmdBindResourceHeapEXT: PFN_vkCmdBindResourceHeapEXT,
        vkCmdBindSamplerHeapEXT: PFN_vkCmdBindSamplerHeapEXT,
        vkCmdPushDataEXT: PFN_vkCmdPushDataEXT,
        // --- resource ---
        vkCreateImage: PFN_vkCreateImage,
        vkDestroyImage: PFN_vkDestroyImage,
        vkGetDeviceImageMemoryRequirements: PFN_vkGetDeviceImageMemoryRequirements,
        vkGetImageMemoryRequirements2: PFN_vkGetImageMemoryRequirements2,
        vkBindImageMemory2: PFN_vkBindImageMemory2,
        vkCreateImageView: PFN_vkCreateImageView,
        vkDestroyImageView: PFN_vkDestroyImageView,
        vkCmdCopyBufferToImage2: PFN_vkCmdCopyBufferToImage2,
        vkCmdCopyImageToBuffer2: PFN_vkCmdCopyImageToBuffer2,
        vkCmdCopyImage2: PFN_vkCmdCopyImage2,
        vkCmdResolveImage2: PFN_vkCmdResolveImage2,
        vkCmdClearColorImage: PFN_vkCmdClearColorImage,
        vkGetImageSparseMemoryRequirements2: PFN_vkGetImageSparseMemoryRequirements2,
        vkQueueBindSparse: PFN_vkQueueBindSparse,
        // --- pipeline and draw ---
        vkCreateGraphicsPipelines: PFN_vkCreateGraphicsPipelines,
        vkCmdBeginRendering: PFN_vkCmdBeginRendering,
        vkCmdEndRendering: PFN_vkCmdEndRendering,
        vkCmdSetViewportWithCount: PFN_vkCmdSetViewportWithCount,
        vkCmdSetScissorWithCount: PFN_vkCmdSetScissorWithCount,
        vkCmdSetStencilReference: PFN_vkCmdSetStencilReference,
        vkCmdSetBlendConstants: PFN_vkCmdSetBlendConstants,
        vkCmdSetDepthBounds: PFN_vkCmdSetDepthBounds,
        vkCmdBindIndexBuffer2: PFN_vkCmdBindIndexBuffer2,
        vkCmdDraw: PFN_vkCmdDraw,
        vkCmdDrawIndexed: PFN_vkCmdDrawIndexed,
        vkCmdDrawMeshTasksEXT: PFN_vkCmdDrawMeshTasksEXT,
        vkCreateIndirectCommandsLayoutEXT: PFN_vkCreateIndirectCommandsLayoutEXT,
        vkDestroyIndirectCommandsLayoutEXT: PFN_vkDestroyIndirectCommandsLayoutEXT,
        vkGetGeneratedCommandsMemoryRequirementsEXT: PFN_vkGetGeneratedCommandsMemoryRequirementsEXT,
        vkCmdExecuteGeneratedCommandsEXT: PFN_vkCmdExecuteGeneratedCommandsEXT,
    }
    optional {
        vkGetDeviceFaultInfoEXT: PFN_vkGetDeviceFaultInfoEXT,
        vkSetDeviceMemoryPriorityEXT: PFN_vkSetDeviceMemoryPriorityEXT,
        // --- resource ---
        // --- pipeline and draw ---
        vkCmdSetFragmentShadingRateKHR: PFN_vkCmdSetFragmentShadingRateKHR,
        // --- query, ray tracing, diagnostics ---
        // --- swapchain and sharing ---
    }
}
