//! Memory: picking memory types for [`MemoryLocation`]s, the shared allocator over them
//! (docs/design/0017-rhi-memory.md) and its blocks, dedicated allocations, and [`Heap`]s for
//! placed and sparse resources.

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use alloc::sync::Arc;

use super::device::{Device, Shared};
use super::ffi::*;
use super::instance::InstanceShared;
use crate::memory::{Allocation, Allocator, MemoryBackend, MemoryType, ResourceClass};
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

    /// The allocator over these types (0017 §3). Blocks grow to 256 MiB in device-local memory
    /// and 64 MiB in host memory (staging and readback, mostly small); heaps of 1 GiB or less
    /// (the 256 MiB BAR window, small integrated carve-outs) get blocks of an eighth of the heap.
    /// Buffers and images share blocks only where `bufferImageGranularity` is 1 (every image is
    /// optimal-tiling, every buffer linear).
    pub(crate) fn allocator(&self, limits: &VkPhysicalDeviceLimits) -> VkAllocator {
        let types = self
            .types()
            .map(|(_, t)| {
                let heap = self.properties.memoryHeaps[t.heapIndex as usize];
                let local = heap.flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT != 0;
                let block_size = match (heap.size, t.propertyFlags & DEVICE_LOCAL != 0) {
                    (size, _) if size <= 1 << 30 => size / 8,
                    (_, true) => 256 << 20,
                    (_, false) => 64 << 20,
                };
                MemoryType {
                    local,
                    block_size: block_size.max(1 << 20),
                }
            })
            .collect();
        let groups = match limits.bufferImageGranularity {
            0 | 1 => [0, 0, 0],
            _ => [0, 1, 1],
        };
        Allocator::new(types, groups)
    }
}

pub(crate) type VkAllocator = Allocator<VkDeviceMemory>;

/// Blocks for the allocator: plain allocations (device addresses allowed, never exportable),
/// mapped whole if host-visible.
impl MemoryBackend for Shared {
    type Raw = VkDeviceMemory;

    fn allocate_block(
        &self,
        memory_type: u32,
        size: u64,
    ) -> Result<(VkDeviceMemory, Option<NonNull<u8>>)> {
        allocate_raw(self, size, memory_type, Dedicated::None, ptr::null())
    }

    unsafe fn free_block(&self, raw: VkDeviceMemory) {
        // SAFETY: the caller's contract; freeing unmaps it.
        unsafe { (self.fns.vkFreeMemory)(self.raw, raw, ptr::null()) };
    }
}

/// How a resource's memory is allocated: [`Dedicated`] when the driver requires or prefers it
/// (`VkMemoryDedicatedRequirements`) or the resource is large, else in a block.
pub(crate) enum Bound {
    Dedicated(Memory),
    Pooled(Allocation<VkDeviceMemory>),
}

impl Bound {
    /// Memory of `memory_type` for a resource with `requirements`. `dedicated` names the
    /// resource, used if it gets memory of its own; `force_dedicated` for exported and imported
    /// memory (`next` holds their structures, else null).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn allocate(
        shared: &Arc<Shared>,
        requirements: &VkMemoryRequirements,
        driver_prefers: bool,
        memory_type: u32,
        class: ResourceClass,
        dedicated: Dedicated,
        force_dedicated: bool,
        next: *const c_void,
    ) -> Result<Self> {
        let size = requirements.size;
        if force_dedicated
            || shared
                .allocator
                .wants_dedicated(memory_type, size, driver_prefers)
        {
            return Memory::allocate(shared, size, memory_type, dedicated, next)
                .map(Self::Dedicated);
        }
        let align = requirements.alignment.max(1).next_power_of_two();
        shared
            .allocator
            .allocate(&**shared, memory_type, class, size, align)
            .map(Self::Pooled)
    }

    /// The memory and offset to bind the resource at.
    pub(crate) fn binding(&self) -> (VkDeviceMemory, u64) {
        match self {
            Self::Dedicated(m) => (m.raw, 0),
            Self::Pooled(a) => (a.raw, a.offset),
        }
    }

    pub(crate) fn mapped(&self) -> Option<NonNull<u8>> {
        match self {
            Self::Dedicated(m) => m.mapped,
            Self::Pooled(a) => a.mapped,
        }
    }

    /// What a resource keeps: its dedicated memory, or its suballocation (freed by the resource's
    /// `Drop` after the resource is destroyed, with [`VkAllocator::free`]).
    pub(crate) fn into_parts(self) -> (Option<Arc<Memory>>, Option<Allocation<VkDeviceMemory>>) {
        match self {
            Self::Dedicated(m) => (Some(Arc::new(m)), None),
            Self::Pooled(a) => (None, Some(a)),
        }
    }

    /// Gives the memory back when binding failed.
    ///
    /// # Safety
    /// No resource is bound to it.
    pub(crate) unsafe fn discard(self, shared: &Shared) {
        if let Self::Pooled(a) = self {
            // SAFETY: the caller's contract.
            unsafe { shared.allocator.free(shared, a) };
        }
    }
}

/// `VkMemoryRequirements2` with the dedicated-allocation preference chained.
pub(crate) struct Requirements {
    pub(crate) memory: VkMemoryRequirements,
    /// The driver requires or prefers memory of the resource's own.
    pub(crate) prefers_dedicated: bool,
}

pub(crate) fn buffer_requirements(shared: &Shared, buffer: VkBuffer) -> Requirements {
    let info = VkBufferMemoryRequirementsInfo2 {
        buffer,
        ..Default::default()
    };
    let mut dedicated = VkMemoryDedicatedRequirements::default();
    let mut req = VkMemoryRequirements2 {
        pNext: (&raw mut dedicated).cast(),
        ..Default::default()
    };
    // SAFETY: a live buffer and valid structures.
    unsafe { (shared.fns.vkGetBufferMemoryRequirements2)(shared.raw, &info, &mut req) };
    Requirements {
        memory: req.memoryRequirements,
        prefers_dedicated: dedicated.prefersDedicatedAllocation != 0
            || dedicated.requiresDedicatedAllocation != 0,
    }
}

pub(crate) fn image_requirements(shared: &Shared, image: VkImage) -> Requirements {
    let info = VkImageMemoryRequirementsInfo2 {
        image,
        ..Default::default()
    };
    let mut dedicated = VkMemoryDedicatedRequirements::default();
    let mut req = VkMemoryRequirements2 {
        pNext: (&raw mut dedicated).cast(),
        ..Default::default()
    };
    // SAFETY: a live image and valid structures.
    unsafe { (shared.fns.vkGetImageMemoryRequirements2)(shared.raw, &info, &mut req) };
    Requirements {
        memory: req.memoryRequirements,
        prefers_dedicated: dedicated.prefersDedicatedAllocation != 0
            || dedicated.requiresDedicatedAllocation != 0,
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
    pub(crate) memory_type: u32,
    size: u64,
    /// The whole allocation's CPU mapping, for host-visible memory.
    pub(crate) mapped: Option<NonNull<u8>>,
    /// Allocated with `VkExportMemoryAllocateInfo`: OS handles can be made of it (share.rs).
    pub(crate) exportable: bool,
}

/// Whether a `pNext` chain holds a structure of type `ty`.
///
/// # Safety
/// `next` is null or a valid chain of Vulkan structures.
unsafe fn chain_has(mut next: *const c_void, ty: VkStructureType) -> bool {
    /// The header every chained structure starts with (`VkBaseInStructure`).
    #[repr(C)]
    struct Base {
        s_type: VkStructureType,
        next: *const c_void,
    }
    while !next.is_null() {
        // SAFETY: every structure in a valid chain starts with this header.
        let base = unsafe { &*next.cast::<Base>() };
        if base.s_type == ty {
            return true;
        }
        next = base.next;
    }
    false
}

// SAFETY: device memory and its mapping may be used from any thread; the mapping is only read
// and written through the public API's unsafe accessors.
unsafe impl Send for Memory {}
// SAFETY: as above.
unsafe impl Sync for Memory {}

/// Allocates `size` bytes of `memory_type`, mapped whole if host-visible. Buffers get device
/// addresses, so every allocation allows them. `next` is chained onto the allocate info.
fn allocate_raw(
    shared: &Shared,
    size: u64,
    memory_type: u32,
    dedicated: Dedicated,
    next: *const c_void,
) -> Result<(VkDeviceMemory, Option<NonNull<u8>>)> {
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
    let mut mapped = None;
    if shared.memory.host_visible(memory_type) {
        let mut data = ptr::null_mut();
        // SAFETY: host-visible memory, mapped whole once.
        let r =
            unsafe { (shared.fns.vkMapMemory)(shared.raw, raw, 0, VK_WHOLE_SIZE, 0, &mut data) };
        if let Err(e) = shared.lost.result(r) {
            // SAFETY: memory nothing uses.
            unsafe { (shared.fns.vkFreeMemory)(shared.raw, raw, ptr::null()) };
            return Err(e);
        }
        mapped = NonNull::new(data.cast());
    }
    Ok((raw, mapped))
}

impl Memory {
    /// A driver allocation of its own: dedicated to a resource (`dedicated`), a heap, or
    /// exported or imported memory (`next` holds those structures, else null). Counted in the
    /// allocator's stats.
    pub(crate) fn allocate(
        shared: &Arc<Shared>,
        size: u64,
        memory_type: u32,
        dedicated: Dedicated,
        next: *const c_void,
    ) -> Result<Self> {
        let (raw, mapped) = allocate_raw(shared, size, memory_type, dedicated, next)?;
        shared.allocator.dedicated_allocated(memory_type, size);
        Ok(Self {
            shared: shared.clone(),
            raw,
            memory_type,
            size,
            mapped,
            // SAFETY: the caller's chain, which the allocation just accepted.
            exportable: unsafe { chain_has(next, VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO) },
        })
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let s = &self.shared;
        s.allocator.dedicated_freed(self.memory_type, self.size);
        // SAFETY: nothing uses the memory any more (its resources are gone); freeing unmaps it.
        unsafe { (s.fns.vkFreeMemory)(s.raw, self.raw, ptr::null()) };
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
