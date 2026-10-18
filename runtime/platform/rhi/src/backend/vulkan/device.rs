//! Device creation: picking the adapter, the baseline check, extensions and features, queue
//! families, and what the rest of the backend shares through [`Shared`].

use core::ffi::{CStr, c_char};
use core::mem::ManuallyDrop;
use core::ops::Deref;
use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::descriptor::Bindless;
use super::diag::Lost;
use super::ffi::*;
use super::instance::{Adapter, Instance, InstanceShared};
use super::memory::MemoryInfo;
use super::queue::Queue;
use super::resource::InternalWork;
use super::{check, unsupported};
use crate::contract::Queues;
use crate::{
    AdapterInfo, AdapterKind, AdapterPreference, DeviceDesc, Error, ErrorKind, Features, QueueType,
    Result, VariableRateShading,
};

/// Device extensions: the baseline's (required) and the optional ones each area uses.
#[derive(Clone, Copy, Default)]
pub(crate) struct DeviceExtensions {
    pub(crate) descriptor_heap: bool,
    pub(crate) shader_untyped_pointers: bool,
    pub(crate) mesh_shader: bool,
    pub(crate) device_generated_commands: bool,
    pub(crate) swapchain: bool,
    pub(crate) swapchain_maintenance1: bool,
    pub(crate) present_id2: bool,
    pub(crate) present_wait2: bool,
    pub(crate) hdr_metadata: bool,
    pub(crate) acceleration_structure: bool,
    pub(crate) deferred_host_operations: bool,
    pub(crate) ray_query: bool,
    pub(crate) fragment_shading_rate: bool,
    pub(crate) conservative_rasterization: bool,
    pub(crate) fragment_shader_barycentric: bool,
    pub(crate) conditional_rendering: bool,
    pub(crate) memory_budget: bool,
    pub(crate) memory_priority: bool,
    pub(crate) pageable_device_local_memory: bool,
    pub(crate) calibrated_timestamps: bool,
    pub(crate) device_fault: bool,
    pub(crate) diagnostic_checkpoints: bool,
    pub(crate) buffer_marker: bool,
    pub(crate) depth_clip_enable: bool,
    pub(crate) external_memory_win32: bool,
    pub(crate) external_semaphore_win32: bool,
    pub(crate) external_memory_fd: bool,
    pub(crate) external_semaphore_fd: bool,
}

impl DeviceExtensions {
    fn table(&mut self) -> [(&'static CStr, &mut bool); 28] {
        [
            (c"VK_EXT_descriptor_heap", &mut self.descriptor_heap),
            (
                c"VK_KHR_shader_untyped_pointers",
                &mut self.shader_untyped_pointers,
            ),
            (c"VK_EXT_mesh_shader", &mut self.mesh_shader),
            (
                c"VK_EXT_device_generated_commands",
                &mut self.device_generated_commands,
            ),
            (c"VK_KHR_swapchain", &mut self.swapchain),
            (
                c"VK_KHR_swapchain_maintenance1",
                &mut self.swapchain_maintenance1,
            ),
            (c"VK_KHR_present_id2", &mut self.present_id2),
            (c"VK_KHR_present_wait2", &mut self.present_wait2),
            (c"VK_EXT_hdr_metadata", &mut self.hdr_metadata),
            (
                c"VK_KHR_acceleration_structure",
                &mut self.acceleration_structure,
            ),
            (
                c"VK_KHR_deferred_host_operations",
                &mut self.deferred_host_operations,
            ),
            (c"VK_KHR_ray_query", &mut self.ray_query),
            (
                c"VK_KHR_fragment_shading_rate",
                &mut self.fragment_shading_rate,
            ),
            (
                c"VK_EXT_conservative_rasterization",
                &mut self.conservative_rasterization,
            ),
            (
                c"VK_KHR_fragment_shader_barycentric",
                &mut self.fragment_shader_barycentric,
            ),
            (
                c"VK_EXT_conditional_rendering",
                &mut self.conditional_rendering,
            ),
            (c"VK_EXT_memory_budget", &mut self.memory_budget),
            (c"VK_EXT_memory_priority", &mut self.memory_priority),
            (
                c"VK_EXT_pageable_device_local_memory",
                &mut self.pageable_device_local_memory,
            ),
            (
                c"VK_KHR_calibrated_timestamps",
                &mut self.calibrated_timestamps,
            ),
            (c"VK_EXT_device_fault", &mut self.device_fault),
            (
                c"VK_NV_device_diagnostic_checkpoints",
                &mut self.diagnostic_checkpoints,
            ),
            (c"VK_AMD_buffer_marker", &mut self.buffer_marker),
            (c"VK_EXT_depth_clip_enable", &mut self.depth_clip_enable),
            (
                c"VK_KHR_external_memory_win32",
                &mut self.external_memory_win32,
            ),
            (
                c"VK_KHR_external_semaphore_win32",
                &mut self.external_semaphore_win32,
            ),
            (c"VK_KHR_external_memory_fd", &mut self.external_memory_fd),
            (
                c"VK_KHR_external_semaphore_fd",
                &mut self.external_semaphore_fd,
            ),
        ]
    }

    /// Drops extensions whose prerequisites are missing.
    fn settle(&mut self) {
        self.acceleration_structure &= self.deferred_host_operations;
        self.deferred_host_operations &= self.acceleration_structure;
        self.ray_query &= self.acceleration_structure;
        self.pageable_device_local_memory &= self.memory_priority;
        self.swapchain_maintenance1 &= self.swapchain;
        self.present_id2 &= self.swapchain;
        self.present_wait2 &= self.present_id2;
        self.hdr_metadata &= self.swapchain;
    }
}

/// Queue families: graphics always; compute-only and transfer-only ones where they exist.
#[derive(Clone)]
pub(crate) struct Families {
    pub(crate) graphics: u32,
    pub(crate) compute: Option<u32>,
    pub(crate) copy: Option<u32>,
    /// The family whose second queue runs the backend's own transitions (resource.rs's
    /// `InternalWork`), so they never queue behind application work waiting on something; `None`
    /// when no family in use has a second queue.
    pub(crate) internal: Option<u32>,
    /// Every family in use, for `VK_SHARING_MODE_CONCURRENT` resources.
    pub(crate) unique: Vec<u32>,
    /// `timestampValidBits` and `queueFlags` per family index.
    pub(crate) properties: Vec<VkQueueFamilyProperties>,
}

impl Families {
    pub(crate) fn of(&self, ty: QueueType) -> u32 {
        match ty {
            QueueType::Graphics => self.graphics,
            QueueType::Compute => self.compute.unwrap_or(self.graphics),
            QueueType::Copy => self.copy.unwrap_or(self.graphics),
        }
    }

    /// The sharing mode and family list for a new buffer or image.
    pub(crate) fn sharing(&self) -> (VkSharingMode, u32, *const u32) {
        match self.unique.len() {
            1 => (VK_SHARING_MODE_EXCLUSIVE, 0, ptr::null()),
            n => (VK_SHARING_MODE_CONCURRENT, n as u32, self.unique.as_ptr()),
        }
    }
}

/// Device properties the backend uses after creation.
#[derive(Clone, Copy)]
pub(crate) struct Properties {
    pub(crate) limits: VkPhysicalDeviceLimits,
    pub(crate) sparse: VkPhysicalDeviceSparseProperties,
    pub(crate) heap: VkPhysicalDeviceDescriptorHeapPropertiesEXT,
    pub(crate) shading_rate: VkPhysicalDeviceFragmentShadingRatePropertiesKHR,
    pub(crate) subgroup_size: u32,
    pub(crate) min_subgroup_size: u32,
    pub(crate) max_subgroup_size: u32,
    pub(crate) pipeline_cache_uuid: [u8; VK_UUID_SIZE],
    pub(crate) vendor_id: u32,
    pub(crate) device_id: u32,
}

/// What every object of a device shares; the `VkDevice` is destroyed when the last goes.
pub(crate) struct Shared {
    pub(crate) raw: VkDevice,
    pub(crate) physical: VkPhysicalDevice,
    pub(crate) fns: DeviceFns,
    pub(crate) instance: Arc<InstanceShared>,
    pub(crate) info: AdapterInfo,
    pub(crate) features: Features,
    pub(crate) properties: Properties,
    pub(crate) ext: DeviceExtensions,
    pub(crate) families: Families,
    pub(crate) memory: MemoryInfo,
    pub(crate) lost: Lost,
    /// Dropped before the device, in `Drop`.
    pub(crate) bindless: ManuallyDrop<Bindless>,
    /// New textures' first layout and sparse binds, which submissions wait for (resource.rs);
    /// destroyed before the device, in `Drop`.
    pub(crate) internal: InternalWork,
}

// SAFETY: Vulkan devices may be used from any thread; objects needing external synchronization
// (queues, command pools) are guarded by their owners.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: every object of the device is gone (they hold an `Arc` to this), and the
        // bindless heaps are dropped once, here, before the device.
        unsafe {
            (self.fns.vkDeviceWaitIdle)(self.raw);
            ManuallyDrop::drop(&mut self.bindless);
            self.internal.destroy(&self.fns, self.raw);
            (self.fns.vkDestroyDevice)(self.raw, ptr::null());
        }
    }
}

/// The public `Device`'s backend half.
pub(crate) struct Device {
    pub(crate) shared: Arc<Shared>,
}

impl Deref for Device {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

/// The feature structures of the baseline and the optional extensions, chained for
/// `vkGetPhysicalDeviceFeatures2` and then, filtered, for `vkCreateDevice`.
#[derive(Default)]
struct FeatureChain {
    core: VkPhysicalDeviceFeatures2,
    v11: VkPhysicalDeviceVulkan11Features,
    v12: VkPhysicalDeviceVulkan12Features,
    v13: VkPhysicalDeviceVulkan13Features,
    v14: VkPhysicalDeviceVulkan14Features,
    heap: VkPhysicalDeviceDescriptorHeapFeaturesEXT,
    untyped: VkPhysicalDeviceShaderUntypedPointersFeaturesKHR,
    mesh: VkPhysicalDeviceMeshShaderFeaturesEXT,
    dgc: VkPhysicalDeviceDeviceGeneratedCommandsFeaturesEXT,
    acceleration: VkPhysicalDeviceAccelerationStructureFeaturesKHR,
    ray_query: VkPhysicalDeviceRayQueryFeaturesKHR,
    shading_rate: VkPhysicalDeviceFragmentShadingRateFeaturesKHR,
    barycentric: VkPhysicalDeviceFragmentShaderBarycentricFeaturesKHR,
    conditional: VkPhysicalDeviceConditionalRenderingFeaturesEXT,
    fault: VkPhysicalDeviceFaultFeaturesEXT,
    priority: VkPhysicalDeviceMemoryPriorityFeaturesEXT,
    pageable: VkPhysicalDevicePageableDeviceLocalMemoryFeaturesEXT,
    depth_clip: VkPhysicalDeviceDepthClipEnableFeaturesEXT,
    present_id2: VkPhysicalDevicePresentId2FeaturesKHR,
    present_wait2: VkPhysicalDevicePresentWait2FeaturesKHR,
    swapchain_maintenance1: VkPhysicalDeviceSwapchainMaintenance1FeaturesKHR,
}

impl FeatureChain {
    /// Links the structures of the extensions in `ext` behind `core`. The chain points into
    /// `self`, so `self` must not move while it is in use.
    fn link(&mut self, ext: &DeviceExtensions) {
        let mut next: *mut core::ffi::c_void = ptr::null_mut();
        macro_rules! push {
            ($field:ident) => {{
                self.$field.pNext = next;
                next = (&raw mut self.$field).cast();
            }};
        }
        if ext.swapchain_maintenance1 {
            push!(swapchain_maintenance1);
        }
        if ext.present_wait2 {
            push!(present_wait2);
        }
        if ext.present_id2 {
            push!(present_id2);
        }
        if ext.depth_clip_enable {
            push!(depth_clip);
        }
        if ext.pageable_device_local_memory {
            push!(pageable);
        }
        if ext.memory_priority {
            push!(priority);
        }
        if ext.device_fault {
            push!(fault);
        }
        if ext.conditional_rendering {
            push!(conditional);
        }
        if ext.fragment_shader_barycentric {
            push!(barycentric);
        }
        if ext.fragment_shading_rate {
            push!(shading_rate);
        }
        if ext.ray_query {
            push!(ray_query);
        }
        if ext.acceleration_structure {
            push!(acceleration);
        }
        push!(dgc);
        push!(mesh);
        push!(untyped);
        push!(heap);
        push!(v14);
        push!(v13);
        push!(v12);
        push!(v11);
        self.core.pNext = next;
    }

    /// Turns off what costs performance for nothing the RHI uses: robustness and capture/replay.
    fn trim(&mut self) {
        self.core.features.robustBufferAccess = VK_FALSE;
        self.v12.bufferDeviceAddressCaptureReplay = VK_FALSE;
        self.v12.bufferDeviceAddressMultiDevice = VK_FALSE;
        self.v13.robustImageAccess = VK_FALSE;
        self.heap.descriptorHeapCaptureReplay = VK_FALSE;
        self.acceleration.accelerationStructureCaptureReplay = VK_FALSE;
    }

    /// The baseline's features, naming the first missing one.
    fn baseline(&self) -> core::result::Result<(), &'static str> {
        let required = [
            (self.v11.shaderDrawParameters, "shaderDrawParameters"),
            (self.v12.timelineSemaphore, "timelineSemaphore"),
            (self.v12.bufferDeviceAddress, "bufferDeviceAddress"),
            (self.v12.runtimeDescriptorArray, "runtimeDescriptorArray"),
            // Shaders declare their own buffer offsets (0016 §4.2).
            (self.v12.scalarBlockLayout, "scalarBlockLayout"),
            (self.v13.synchronization2, "synchronization2"),
            (self.v13.dynamicRendering, "dynamicRendering"),
            (self.v13.maintenance4, "maintenance4"),
            (self.v14.maintenance5, "maintenance5"),
            (self.v14.maintenance6, "maintenance6"),
            (self.heap.descriptorHeap, "descriptorHeap"),
            // Shaders reach the descriptor heaps through untyped pointers.
            (self.untyped.shaderUntypedPointers, "shaderUntypedPointers"),
            (self.mesh.taskShader, "taskShader"),
            (self.mesh.meshShader, "meshShader"),
            (self.dgc.deviceGeneratedCommands, "deviceGeneratedCommands"),
            (
                self.core.features.shaderStorageImageWriteWithoutFormat,
                "shaderStorageImageWriteWithoutFormat",
            ),
        ];
        match required.iter().find(|(on, _)| *on == VK_FALSE) {
            Some((_, name)) => Err(name),
            None => Ok(()),
        }
    }
}

/// The device's extensions, or `None` if it lacks a required one.
fn extensions(instance: &InstanceShared, physical: VkPhysicalDevice) -> Option<DeviceExtensions> {
    let f = &instance.fns;
    let mut count = 0;
    // SAFETY: the two-call idiom with valid pointers.
    unsafe {
        (f.vkEnumerateDeviceExtensionProperties)(physical, ptr::null(), &mut count, ptr::null_mut())
    };
    let mut available = alloc::vec![VkExtensionProperties::default(); count as usize];
    // SAFETY: as above; `available` holds `count` entries.
    unsafe {
        (f.vkEnumerateDeviceExtensionProperties)(
            physical,
            ptr::null(),
            &mut count,
            available.as_mut_ptr(),
        )
    };
    available.truncate(count as usize);
    let mut ext = DeviceExtensions::default();
    for (name, on) in ext.table() {
        *on = available.iter().any(|e| name_of(&e.extensionName) == name);
    }
    ext.settle();
    (ext.descriptor_heap
        && ext.shader_untyped_pointers
        && ext.mesh_shader
        && ext.device_generated_commands)
        .then_some(ext)
}

fn queue_families(instance: &InstanceShared, physical: VkPhysicalDevice) -> Option<Families> {
    let f = &instance.fns;
    let mut count = 0;
    // SAFETY: the two-call idiom with valid pointers.
    unsafe { (f.vkGetPhysicalDeviceQueueFamilyProperties)(physical, &mut count, ptr::null_mut()) };
    let mut properties = alloc::vec![VkQueueFamilyProperties::default(); count as usize];
    // SAFETY: as above.
    unsafe {
        (f.vkGetPhysicalDeviceQueueFamilyProperties)(physical, &mut count, properties.as_mut_ptr())
    };
    let find = |want: VkQueueFlags, without: VkQueueFlags| {
        properties
            .iter()
            .position(|p| {
                p.queueCount > 0 && p.queueFlags & want == want && p.queueFlags & without == 0
            })
            .map(|i| i as u32)
    };
    let graphics = find(VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT, 0)?;
    let compute = find(VK_QUEUE_COMPUTE_BIT, VK_QUEUE_GRAPHICS_BIT);
    let copy = find(
        VK_QUEUE_TRANSFER_BIT,
        VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT,
    );
    // Graphics first: its queues take every command; then compute, then copy.
    let internal = [Some(graphics), compute, copy]
        .into_iter()
        .flatten()
        .find(|&f| properties[f as usize].queueCount >= 2);
    let mut unique = alloc::vec![graphics];
    unique.extend(compute);
    unique.extend(copy);
    Some(Families {
        graphics,
        compute,
        copy,
        internal,
        unique,
        properties,
    })
}

fn properties(
    instance: &InstanceShared,
    physical: VkPhysicalDevice,
    ext: &DeviceExtensions,
) -> Properties {
    let mut heap = VkPhysicalDeviceDescriptorHeapPropertiesEXT::default();
    let mut shading_rate = VkPhysicalDeviceFragmentShadingRatePropertiesKHR::default();
    let mut v11 = VkPhysicalDeviceVulkan11Properties::default();
    let mut v13 = VkPhysicalDeviceVulkan13Properties {
        pNext: (&raw mut v11).cast(),
        ..Default::default()
    };
    heap.pNext = (&raw mut v13).cast();
    let mut next: *mut core::ffi::c_void = (&raw mut heap).cast();
    if ext.fragment_shading_rate {
        shading_rate.pNext = next;
        next = (&raw mut shading_rate).cast();
    }
    let mut props = VkPhysicalDeviceProperties2 {
        pNext: next,
        ..Default::default()
    };
    // SAFETY: a physical device of this instance and a valid output chain.
    unsafe { (instance.fns.vkGetPhysicalDeviceProperties2)(physical, &mut props) };
    let p = &props.properties;
    Properties {
        limits: p.limits,
        sparse: p.sparseProperties,
        heap,
        shading_rate,
        subgroup_size: v11.subgroupSize,
        min_subgroup_size: v13.minSubgroupSize,
        max_subgroup_size: v13.maxSubgroupSize,
        pipeline_cache_uuid: p.pipelineCacheUUID,
        vendor_id: p.vendorID,
        device_id: p.deviceID,
    }
}

/// Whether `adapter` meets the baseline, logging why not.
fn suitable(instance: &InstanceShared, adapter: &Adapter) -> Option<(DeviceExtensions, Families)> {
    let name = adapter.info.name();
    if adapter.api_version < API_VERSION_1_4 {
        marlin_diag::debug!("Vulkan adapter {name}: below Vulkan 1.4");
        return None;
    }
    let Some(ext) = extensions(instance, adapter.physical) else {
        marlin_diag::debug!("Vulkan adapter {name}: lacks a baseline extension");
        return None;
    };
    let families = queue_families(instance, adapter.physical)?;
    let mut chain = FeatureChain::default();
    chain.link(&ext);
    // SAFETY: a valid output chain.
    unsafe { (instance.fns.vkGetPhysicalDeviceFeatures2)(adapter.physical, &mut chain.core) };
    if let Err(missing) = chain.baseline() {
        marlin_diag::debug!("Vulkan adapter {name}: lacks {missing}");
        return None;
    }
    Some((ext, families))
}

/// Pixels per side of a shading-rate image texel: the smallest square size the device takes
/// among 8, 16 and 32.
fn shading_rate_tile_size(p: &VkPhysicalDeviceFragmentShadingRatePropertiesKHR) -> u32 {
    let (min, max) = (
        p.minFragmentShadingRateAttachmentTexelSize,
        p.maxFragmentShadingRateAttachmentTexelSize,
    );
    [8, 16, 32]
        .into_iter()
        .find(|&s| s >= min.width && s >= min.height && s <= max.width && s <= max.height)
        .unwrap_or(0)
}

fn report_features(
    chain: &FeatureChain,
    ext: &DeviceExtensions,
    props: &Properties,
    families: &Families,
    memory: &MemoryInfo,
    tearing: bool,
) -> Features {
    let c = &chain.core.features;
    let on = |b: VkBool32| b != VK_FALSE;
    let r = &chain.shading_rate;
    let variable_rate_shading = if !ext.fragment_shading_rate || !on(r.pipelineFragmentShadingRate)
    {
        VariableRateShading::None
    } else if on(r.primitiveFragmentShadingRate)
        && on(r.attachmentFragmentShadingRate)
        && shading_rate_tile_size(&props.shading_rate) != 0
    {
        VariableRateShading::Image
    } else {
        VariableRateShading::PerDraw
    };
    let s = &props.sparse;
    let sparse_queue = families.properties[families.graphics as usize].queueFlags
        & VK_QUEUE_SPARSE_BINDING_BIT
        != 0;
    let sparse_resources = sparse_queue
        && on(c.sparseBinding)
        && on(c.sparseResidencyBuffer)
        && on(c.sparseResidencyImage2D)
        && on(c.shaderResourceResidency)
        && on(s.residencyStandard2DBlockShape)
        && on(s.residencyNonResidentStrict);
    // Transfer queues write timestamps but cannot copy query results out or reset queries
    // (vkCmdCopyQueryPoolResults and vkCmdResetQueryPool are graphics and compute commands), so
    // copy lists could never resolve them.
    let copy_queue_timestamps = false;
    let subgroup_control = on(chain.v13.subgroupSizeControl);
    Features {
        // SPIR-V has no shader model; the baseline runs what HLSL shader model 6.6 expresses.
        shader_model: (6, 6),
        enhanced_barriers: true,
        tearing,
        mesh_shader: true,
        ray_query: ext.ray_query
            && on(chain.acceleration.accelerationStructure)
            && on(chain.ray_query.rayQuery),
        shader_float16_int16: on(chain.v12.shaderFloat16) && on(c.shaderInt16),
        shader_int64: on(c.shaderInt64),
        barycentrics: ext.fragment_shader_barycentric
            && on(chain.barycentric.fragmentShaderBarycentric),
        sampler_min_max: on(chain.v12.samplerFilterMinmax),
        conservative_rasterization: ext.conservative_rasterization,
        depth_bounds: on(c.depthBounds),
        variable_rate_shading,
        shading_rate_tile_size: match variable_rate_shading {
            VariableRateShading::Image => shading_rate_tile_size(&props.shading_rate),
            _ => 0,
        },
        pipeline_statistics: on(c.pipelineStatisticsQuery) && on(chain.mesh.meshShaderQueries),
        buffer_markers: ext.buffer_marker,
        sparse_resources,
        sparse_volumes: sparse_resources
            && on(c.sparseResidencyImage3D)
            && on(s.residencyStandard3DBlockShape),
        gpu_upload_heap: memory.resizable_bar(),
        atomic_int64: on(chain.v12.shaderBufferInt64Atomics)
            && on(chain.v12.shaderSharedInt64Atomics),
        copy_queue_timestamps,
        mixed_heaps: true,
        wave_size_range: match subgroup_control {
            true => (props.min_subgroup_size, props.max_subgroup_size),
            false => (props.subgroup_size, props.subgroup_size),
        },
    }
}

pub(crate) fn create(instance: &Instance, desc: &DeviceDesc) -> Result<(Device, Queues<Queue>)> {
    let shared_instance = &instance.shared;
    let not_found = || Error::from_kind(ErrorKind::NotFound);
    fn pick<'a>(
        instance: &InstanceShared,
        adapter: &'a Adapter,
    ) -> Option<(&'a Adapter, (DeviceExtensions, Families))> {
        suitable(instance, adapter).map(|s| (adapter, s))
    }
    let pick = |a| pick(shared_instance, a);
    let (adapter, (ext, families)) = match desc.adapter {
        AdapterPreference::HighPerformance => instance
            .adapters
            .iter()
            .filter(|a| a.info.kind == AdapterKind::Hardware)
            .find_map(pick)
            .ok_or_else(not_found)?,
        AdapterPreference::Software => instance
            .adapters
            .iter()
            .filter(|a| a.info.kind == AdapterKind::Software)
            .find_map(pick)
            .ok_or_else(not_found)?,
        AdapterPreference::Index(i) => {
            let a = instance.adapters.get(i as usize).ok_or_else(not_found)?;
            pick(a).ok_or_else(unsupported)?
        }
        AdapterPreference::Luid(luid) => {
            let a = instance
                .adapters
                .iter()
                .find(|a| a.info.luid == luid)
                .ok_or_else(not_found)?;
            pick(a).ok_or_else(unsupported)?
        }
    };
    let physical = adapter.physical;
    let fns = &shared_instance.fns;

    let mut chain = FeatureChain::default();
    chain.link(&ext);
    // SAFETY: a valid output chain.
    unsafe { (fns.vkGetPhysicalDeviceFeatures2)(physical, &mut chain.core) };
    chain.trim();

    let props = properties(shared_instance, physical, &ext);
    let memory = MemoryInfo::query(shared_instance, physical);

    let priorities = [1.0f32; 2];
    let queue_infos: Vec<VkDeviceQueueCreateInfo> = families
        .unique
        .iter()
        .map(|&family| VkDeviceQueueCreateInfo {
            queueFamilyIndex: family,
            queueCount: if families.internal == Some(family) {
                2
            } else {
                1
            },
            pQueuePriorities: priorities.as_ptr(),
            ..Default::default()
        })
        .collect();
    let mut ext_copy = ext;
    let names: Vec<*const c_char> = ext_copy
        .table()
        .into_iter()
        .filter(|(_, on)| **on)
        .map(|(name, _)| name.as_ptr())
        .collect();
    let info = VkDeviceCreateInfo {
        pNext: (&raw const chain.core).cast(),
        queueCreateInfoCount: queue_infos.len() as u32,
        pQueueCreateInfos: queue_infos.as_ptr(),
        enabledExtensionCount: names.len() as u32,
        ppEnabledExtensionNames: names.as_ptr(),
        ..Default::default()
    };
    let mut raw = ptr::null_mut();
    // SAFETY: a valid create info chain (the features were queried from this device) and out
    // pointer.
    check(unsafe { (fns.vkCreateDevice)(physical, &info, ptr::null(), &mut raw) })?;

    // SAFETY: device commands are fetched through the new device.
    let device_fns = match unsafe {
        DeviceFns::load(&|name| (fns.vkGetDeviceProcAddr)(raw, name.as_ptr()))
    } {
        Ok(f) => f,
        Err(missing) => {
            marlin_diag::warn!("Vulkan device lacks {missing}");
            // SAFETY: a live device and a NUL-terminated name.
            let destroy = unsafe { (fns.vkGetDeviceProcAddr)(raw, c"vkDestroyDevice".as_ptr()) };
            if let Some(destroy) = destroy {
                // SAFETY: vkDestroyDevice has this signature; nothing uses the new device.
                unsafe {
                    let destroy: PFN_vkDestroyDevice = core::mem::transmute(destroy);
                    destroy(raw, ptr::null());
                }
            }
            return Err(unsupported());
        }
    };

    let bindless = match Bindless::new(&device_fns, raw, &memory, &props) {
        Ok(b) => b,
        Err(e) => {
            // SAFETY: nothing uses the new device.
            unsafe { (device_fns.vkDestroyDevice)(raw, ptr::null()) };
            return Err(e);
        }
    };
    let internal_queue = families.internal.map(|family| {
        let info = VkDeviceQueueInfo2 {
            queueFamilyIndex: family,
            queueIndex: 1,
            ..Default::default()
        };
        let mut queue = ptr::null_mut();
        // SAFETY: the device was created with two queues in this family.
        unsafe { (device_fns.vkGetDeviceQueue2)(raw, &info, &mut queue) };
        (queue, family)
    });
    let internal = match InternalWork::new(&device_fns, raw, internal_queue) {
        Ok(w) => w,
        Err(e) => {
            drop(bindless);
            // SAFETY: nothing uses the new device.
            unsafe { (device_fns.vkDestroyDevice)(raw, ptr::null()) };
            return Err(e);
        }
    };

    let tearing = super::swapchain::tearing(shared_instance, physical);
    let features = report_features(&chain, &ext, &props, &families, &memory, tearing);
    let shared = Arc::new(Shared {
        raw,
        physical,
        fns: device_fns,
        instance: shared_instance.clone(),
        info: adapter.info,
        features,
        properties: props,
        ext,
        families: families.clone(),
        memory,
        lost: Lost::default(),
        bindless: ManuallyDrop::new(bindless),
        internal,
    });
    shared.lost.attach(&shared);
    let queues = Queues {
        graphics: Queue::new(&shared, families.graphics),
        compute: families.compute.map(|f| Queue::new(&shared, f)),
        copy: families.copy.map(|f| Queue::new(&shared, f)),
    };
    let _ = desc.disable_enhanced_barriers; // one barrier path: synchronization2
    Ok((Device { shared }, queues))
}
