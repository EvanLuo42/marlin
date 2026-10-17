//! Memory: picking memory types for [`MemoryLocation`]s, allocations (committed resources get one
//! each), and [`Heap`]s for placed and sparse resources.

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use alloc::sync::Arc;

use super::device::{Device, Shared};
use super::ffi::*;
use super::instance::InstanceShared;
use crate::{Error, ErrorKind, HeapDesc, MemoryLocation, Result};

/// The device's memory types and heaps.
#[derive(Clone, Copy)]
pub(crate) struct MemoryInfo {
    pub(crate) properties: VkPhysicalDeviceMemoryProperties,
}

const DEVICE_LOCAL: u32 = VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT;
const HOST_VISIBLE: u32 = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT;
const HOST_COHERENT: u32 = VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
const HOST_CACHED: u32 = VK_MEMORY_PROPERTY_HOST_CACHED_BIT;

/// Device-local, CPU-visible memory larger than this is resizable BAR (the 256 MiB window is not).
const RESIZABLE_BAR_MIN: u64 = 1 << 30;

impl MemoryInfo {
    pub(crate) fn query(instance: &InstanceShared, physical: VkPhysicalDevice) -> Self {
        let mut p = VkPhysicalDeviceMemoryProperties2::default();
        // SAFETY: a physical device of this instance and a valid output structure.
        unsafe { (instance.fns.vkGetPhysicalDeviceMemoryProperties2)(physical, &mut p) };
        Self {
            properties: p.memoryProperties,
        }
    }

    fn types(&self) -> impl Iterator<Item = (u32, &VkMemoryType)> {
        let p = &self.properties;
        p.memoryTypes[..p.memoryTypeCount as usize]
            .iter()
            .enumerate()
            .map(|(i, t)| (i as u32, t))
    }

    fn heap_size(&self, heap: u32) -> u64 {
        self.properties.memoryHeaps[heap as usize].size
    }

    /// A device-local, CPU-visible type on a heap as big as video memory exists.
    pub(crate) fn resizable_bar(&self) -> bool {
        self.types().any(|(_, t)| {
            t.propertyFlags & (DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT)
                == (DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT)
                && self.heap_size(t.heapIndex) >= RESIZABLE_BAR_MIN
        })
    }

    /// The best memory type for `location` among `allowed` (a `memoryTypeBits` mask): the first
    /// that has every required property and the most preferred ones, avoiding unwanted ones.
    pub(crate) fn find_type(&self, allowed: u32, location: MemoryLocation) -> Option<u32> {
        // (required, preferred, avoided)
        let (required, preferred, avoided) = match location {
            MemoryLocation::GpuOnly => (DEVICE_LOCAL, 0, HOST_VISIBLE),
            MemoryLocation::Upload => (HOST_VISIBLE | HOST_COHERENT, 0, DEVICE_LOCAL | HOST_CACHED),
            MemoryLocation::Readback => (HOST_VISIBLE | HOST_COHERENT, HOST_CACHED, DEVICE_LOCAL),
            MemoryLocation::GpuUpload => {
                (DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT, 0, HOST_CACHED)
            }
        };
        self.types()
            .filter(|(i, t)| allowed & (1 << i) != 0 && t.propertyFlags & required == required)
            .filter(|(_, t)| {
                location != MemoryLocation::GpuUpload
                    || self.heap_size(t.heapIndex) >= RESIZABLE_BAR_MIN
            })
            .max_by_key(|(i, t)| {
                let score = (t.propertyFlags & preferred).count_ones() as i32 * 2
                    - (t.propertyFlags & avoided).count_ones() as i32 * 3;
                // Earlier types win ties (drivers list the best first).
                (score, -(*i as i32))
            })
            .map(|(i, _)| i)
    }

    pub(crate) fn host_visible(&self, memory_type: u32) -> bool {
        self.properties.memoryTypes[memory_type as usize].propertyFlags & HOST_VISIBLE != 0
    }
}

/// What a resource's memory is dedicated to, if anything.
#[derive(Clone, Copy)]
pub(crate) enum Dedicated {
    None,
    Buffer(VkBuffer),
    Image(VkImage),
}

/// A `VkDeviceMemory`, freed (and unmapped) on drop.
pub(crate) struct Memory {
    shared: Arc<Shared>,
    pub(crate) raw: VkDeviceMemory,
    pub(crate) size: u64,
    pub(crate) memory_type: u32,
    /// The whole allocation's CPU mapping, for host-visible memory.
    pub(crate) mapped: Option<NonNull<u8>>,
}

// SAFETY: device memory and its mapping may be used from any thread; the mapping is only read
// and written through the public API's unsafe accessors.
unsafe impl Send for Memory {}
// SAFETY: as above.
unsafe impl Sync for Memory {}

impl Memory {
    /// Allocates `size` bytes of `memory_type` (from `requirements` and a location), mapped if
    /// host-visible. Buffers get device addresses, so every allocation allows them.
    pub(crate) fn allocate(
        shared: &Arc<Shared>,
        size: u64,
        memory_type: u32,
        dedicated: Dedicated,
        next: *const c_void,
    ) -> Result<Self> {
        let mut dedicated_info = VkMemoryDedicatedAllocateInfo {
            pNext: next,
            ..Default::default()
        };
        let mut chain = next;
        match dedicated {
            Dedicated::None => {}
            Dedicated::Buffer(b) => {
                dedicated_info.buffer = b;
                chain = (&raw const dedicated_info).cast();
            }
            Dedicated::Image(i) => {
                dedicated_info.image = i;
                chain = (&raw const dedicated_info).cast();
            }
        }
        let flags = VkMemoryAllocateFlagsInfo {
            pNext: chain,
            flags: VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT,
            ..Default::default()
        };
        let info = VkMemoryAllocateInfo {
            pNext: (&raw const flags).cast(),
            allocationSize: size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid allocate info chain and out pointer.
        shared.lost.result(unsafe {
            (shared.fns.vkAllocateMemory)(shared.raw, &info, ptr::null(), &mut raw)
        })?;
        let mut memory = Self {
            shared: shared.clone(),
            raw,
            size,
            memory_type,
            mapped: None,
        };
        if shared.memory.host_visible(memory_type) {
            let mut data = ptr::null_mut();
            // SAFETY: host-visible memory, mapped whole once.
            shared.lost.result(unsafe {
                (shared.fns.vkMapMemory)(shared.raw, raw, 0, VK_WHOLE_SIZE, 0, &mut data)
            })?;
            memory.mapped = NonNull::new(data.cast());
        }
        Ok(memory)
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: nothing uses the memory any more (its resources are gone); freeing unmaps it.
        unsafe { (self.shared.fns.vkFreeMemory)(self.shared.raw, self.raw, ptr::null()) };
    }
}

/// Memory for placed resources and sparse tiles. Cheap to clone; placed resources keep it alive.
#[derive(Clone)]
pub(crate) struct Heap {
    pub(crate) memory: Arc<Memory>,
}

impl Heap {
    pub(crate) fn new(device: &Device, desc: &HeapDesc) -> Result<Self> {
        let memory_type = device
            .memory
            .find_type(u32::MAX, desc.memory)
            .ok_or(Error::from_kind(ErrorKind::Unsupported))?;
        let memory = Memory::allocate(
            &device.shared,
            desc.size,
            memory_type,
            Dedicated::None,
            ptr::null(),
        )?;
        Ok(Self {
            memory: Arc::new(memory),
        })
    }

    pub(crate) fn raw(&self) -> VkDeviceMemory {
        self.memory.raw
    }

    pub(crate) fn shared(&self) -> &Shared {
        self.memory.shared()
    }
}
