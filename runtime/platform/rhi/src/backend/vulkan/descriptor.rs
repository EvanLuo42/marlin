//! The bindless tables as descriptor heaps (0016 §4): one resource heap and one sampler heap, each
//! a host-visible buffer of fixed-size slots that views write into, bound once per command list.

use core::ffi::c_void;
use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::device::{Properties, Shared};
use super::ffi::*;
use super::memory::MemoryInfo;
use super::{check, unsupported};
use crate::{
    Error, ErrorKind, MAX_BINDLESS_RESOURCES, MAX_BINDLESS_SAMPLERS, MemoryLocation, Result,
};

/// The SPIR-V bindings of the set 0 mapping (0016 §4.2).
pub(crate) const RESOURCE_BINDING: u32 = 0;
pub(crate) const SAMPLER_BINDING: u32 = 1;
pub(crate) const PUSH_BINDING: u32 = 2;

/// Free slots of one heap.
struct Slots {
    free: Vec<u32>,
    next: u32,
    capacity: u32,
}

impl Slots {
    fn take(&mut self) -> Option<u32> {
        self.free.pop().or_else(|| {
            (self.next < self.capacity).then(|| {
                self.next += 1;
                self.next - 1
            })
        })
    }
}

/// One descriptor heap: a buffer of `capacity` slots of `stride` bytes, then the driver's
/// reserved range.
pub(crate) struct DescriptorHeap {
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    mapped: *mut u8,
    address: u64,
    /// Bytes per slot.
    pub(crate) stride: u64,
    capacity: u32,
    reserved_offset: u64,
    reserved_size: u64,
    size: u64,
    slots: Mutex<Slots>,
}

impl DescriptorHeap {
    fn new(
        fns: &DeviceFns,
        device: VkDevice,
        memory: &MemoryInfo,
        capacity: u32,
        stride: u64,
        alignment: u64,
        reserved: u64,
    ) -> Result<Self> {
        let reserved_offset = (stride * capacity as u64).next_multiple_of(alignment.max(1));
        let size = reserved_offset + reserved;
        let usage = VkBufferUsageFlags2CreateInfo {
            usage: VK_BUFFER_USAGE_2_DESCRIPTOR_HEAP_BIT_EXT
                | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT,
            ..Default::default()
        };
        let info = VkBufferCreateInfo {
            pNext: (&raw const usage).cast(),
            size,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            ..Default::default()
        };
        let mut buffer = 0;
        // SAFETY: a valid create info chain and out pointer.
        check(unsafe { (fns.vkCreateBuffer)(device, &info, ptr::null(), &mut buffer) })?;
        let cleanup = |memory: VkDeviceMemory| {
            // SAFETY: the buffer (and memory) were just created and nothing uses them.
            unsafe {
                (fns.vkDestroyBuffer)(device, buffer, ptr::null());
                if memory != 0 {
                    (fns.vkFreeMemory)(device, memory, ptr::null());
                }
            }
        };
        let req_info = VkBufferMemoryRequirementsInfo2 {
            buffer,
            ..Default::default()
        };
        let mut req = VkMemoryRequirements2::default();
        // SAFETY: a live buffer and valid structures.
        unsafe { (fns.vkGetBufferMemoryRequirements2)(device, &req_info, &mut req) };
        // Device-local and CPU-visible where the device has it (the 256 MiB BAR window holds the
        // heaps); system memory otherwise.
        let memory_type = memory
            .find_type(
                req.memoryRequirements.memoryTypeBits,
                MemoryLocation::GpuUpload,
            )
            .or_else(|| {
                let bits = req.memoryRequirements.memoryTypeBits;
                (0..memory.properties.memoryTypeCount).find(|&i| {
                    let flags = memory.properties.memoryTypes[i as usize].propertyFlags;
                    bits & (1 << i) != 0
                        && flags
                            & (VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT
                                | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
                                | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                            == VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT
                                | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
                                | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT
                })
            })
            .or_else(|| {
                memory.find_type(
                    req.memoryRequirements.memoryTypeBits,
                    MemoryLocation::Upload,
                )
            });
        let Some(memory_type) = memory_type else {
            cleanup(0);
            return Err(unsupported());
        };
        let dedicated = VkMemoryDedicatedAllocateInfo {
            buffer,
            ..Default::default()
        };
        let flags = VkMemoryAllocateFlagsInfo {
            pNext: (&raw const dedicated).cast(),
            flags: VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT,
            ..Default::default()
        };
        let alloc_info = VkMemoryAllocateInfo {
            pNext: (&raw const flags).cast(),
            allocationSize: req.memoryRequirements.size,
            memoryTypeIndex: memory_type,
            ..Default::default()
        };
        let mut raw_memory = 0;
        // SAFETY: valid allocate info and out pointer.
        if let Err(e) = check(unsafe {
            (fns.vkAllocateMemory)(device, &alloc_info, ptr::null(), &mut raw_memory)
        }) {
            cleanup(0);
            return Err(e);
        }
        let bind = VkBindBufferMemoryInfo {
            buffer,
            memory: raw_memory,
            ..Default::default()
        };
        let mut mapped = ptr::null_mut();
        // SAFETY: a fresh buffer bound to its dedicated, host-visible memory, mapped whole.
        let result = unsafe {
            check((fns.vkBindBufferMemory2)(device, 1, &bind)).and_then(|()| {
                check((fns.vkMapMemory)(
                    device,
                    raw_memory,
                    0,
                    VK_WHOLE_SIZE,
                    0,
                    &mut mapped,
                ))
            })
        };
        if let Err(e) = result {
            cleanup(raw_memory);
            return Err(e);
        }
        let address_info = VkBufferDeviceAddressInfo {
            buffer,
            ..Default::default()
        };
        // SAFETY: a bound buffer with device address usage.
        let address = unsafe { (fns.vkGetBufferDeviceAddress)(device, &address_info) };
        // Zeroed slots are null descriptors until a view is written.
        // SAFETY: the mapping covers `size` bytes.
        unsafe { ptr::write_bytes(mapped.cast::<u8>(), 0, size as usize) };
        Ok(Self {
            buffer,
            memory: raw_memory,
            mapped: mapped.cast(),
            address,
            stride,
            capacity,
            reserved_offset,
            reserved_size: reserved,
            size,
            slots: Mutex::new(Slots {
                free: Vec::new(),
                next: 0,
                capacity,
            }),
        })
    }

    /// `vkCmdBind*HeapEXT`'s argument.
    pub(crate) fn bind_info(&self) -> VkBindHeapInfoEXT {
        VkBindHeapInfoEXT {
            heapRange: VkDeviceAddressRangeKHR {
                address: self.address,
                size: self.size,
            },
            reservedRangeOffset: self.reserved_offset,
            reservedRangeSize: self.reserved_size,
            ..Default::default()
        }
    }

    /// Where slot `index` lives in the mapping.
    pub(crate) fn slot(&self, index: u32) -> VkHostAddressRangeEXT {
        debug_assert!(index < self.capacity);
        VkHostAddressRangeEXT {
            // SAFETY: in bounds: `index < capacity`.
            address: unsafe { self.mapped.add((index as u64 * self.stride) as usize) }
                .cast::<c_void>(),
            size: self.stride as usize,
        }
    }

    fn take(&self) -> Result<u32> {
        self.slots
            .lock()
            .take()
            .ok_or(Error::from_kind(ErrorKind::OutOfMemory))
    }

    fn free(&self, index: u32) {
        // SAFETY: the slot is the caller's and nothing reads it any more; zero it so a stale index
        // reads a null descriptor.
        unsafe {
            ptr::write_bytes(
                self.slot(index).address.cast::<u8>(),
                0,
                self.stride as usize,
            )
        };
        self.slots.lock().free.push(index);
    }

    /// # Safety
    /// Called once, before the device is destroyed, with nothing using the heap.
    unsafe fn destroy(&self, fns: &DeviceFns, device: VkDevice) {
        // SAFETY: forwarded from the caller; freeing unmaps.
        unsafe {
            (fns.vkDestroyBuffer)(device, self.buffer, ptr::null());
            (fns.vkFreeMemory)(device, self.memory, ptr::null());
        }
    }
}

/// Both heaps, plus what pipelines need to map the set 0 bindings onto them.
pub(crate) struct Bindless {
    pub(crate) resources: DescriptorHeap,
    pub(crate) samplers: DescriptorHeap,
    fns_device: VkDevice,
    destroy_buffer: PFN_vkDestroyBuffer,
    free_memory: PFN_vkFreeMemory,
}

// SAFETY: the mappings are written only through slots the writer owns; the free lists are locked.
unsafe impl Send for Bindless {}
// SAFETY: as above.
unsafe impl Sync for Bindless {}

impl Bindless {
    pub(crate) fn new(
        fns: &DeviceFns,
        device: VkDevice,
        memory: &MemoryInfo,
        props: &Properties,
    ) -> Result<Self> {
        let h = &props.heap;
        let resource_alignment = h
            .imageDescriptorAlignment
            .max(h.bufferDescriptorAlignment)
            .max(1);
        let resource_stride = h
            .imageDescriptorSize
            .max(h.bufferDescriptorSize)
            .next_multiple_of(resource_alignment);
        let sampler_stride = h
            .samplerDescriptorSize
            .next_multiple_of(h.samplerDescriptorAlignment.max(1));
        let resources = DescriptorHeap::new(
            fns,
            device,
            memory,
            MAX_BINDLESS_RESOURCES,
            resource_stride,
            resource_alignment.max(h.resourceHeapAlignment),
            h.minResourceHeapReservedRange,
        )?;
        let samplers = match DescriptorHeap::new(
            fns,
            device,
            memory,
            MAX_BINDLESS_SAMPLERS,
            sampler_stride,
            h.samplerDescriptorAlignment
                .max(h.samplerHeapAlignment)
                .max(1),
            h.minSamplerHeapReservedRange,
        ) {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: just created, unused.
                unsafe { resources.destroy(fns, device) };
                return Err(e);
            }
        };
        if resources.size > h.maxResourceHeapSize || samplers.size > h.maxSamplerHeapSize {
            // SAFETY: just created, unused.
            unsafe {
                resources.destroy(fns, device);
                samplers.destroy(fns, device);
            }
            return Err(unsupported());
        }
        Ok(Self {
            resources,
            samplers,
            fns_device: device,
            destroy_buffer: fns.vkDestroyBuffer,
            free_memory: fns.vkFreeMemory,
        })
    }

    /// The set 0 bindings as the descriptor heap mapping every pipeline is created with: binding
    /// 0 is the resource heap, binding 1 the sampler heap (both indexed by slot), binding 2 the
    /// push data (0016 §4.2).
    pub(crate) fn mappings(&self) -> [VkDescriptorSetAndBindingMappingEXT; 3] {
        let constant = |stride: u64, sampler_stride: u64| VkDescriptorMappingSourceDataEXT {
            constantOffset: VkDescriptorMappingSourceConstantOffsetEXT {
                heapOffset: 0,
                heapArrayStride: stride as u32,
                pEmbeddedSampler: ptr::null(),
                samplerHeapOffset: 0,
                samplerHeapArrayStride: sampler_stride as u32,
            },
        };
        let mapping = |binding, mask, source, data| VkDescriptorSetAndBindingMappingEXT {
            descriptorSet: 0,
            firstBinding: binding,
            bindingCount: 1,
            resourceMask: mask,
            source,
            sourceData: data,
            ..Default::default()
        };
        [
            mapping(
                RESOURCE_BINDING,
                VK_SPIRV_RESOURCE_TYPE_ALL_EXT & !VK_SPIRV_RESOURCE_TYPE_SAMPLER_BIT_EXT,
                VK_DESCRIPTOR_MAPPING_SOURCE_HEAP_WITH_CONSTANT_OFFSET_EXT,
                constant(self.resources.stride, self.samplers.stride),
            ),
            mapping(
                SAMPLER_BINDING,
                VK_SPIRV_RESOURCE_TYPE_SAMPLER_BIT_EXT,
                VK_DESCRIPTOR_MAPPING_SOURCE_HEAP_WITH_CONSTANT_OFFSET_EXT,
                constant(self.samplers.stride, self.samplers.stride),
            ),
            mapping(
                PUSH_BINDING,
                VK_SPIRV_RESOURCE_TYPE_UNIFORM_BUFFER_BIT_EXT,
                VK_DESCRIPTOR_MAPPING_SOURCE_PUSH_DATA_EXT,
                VkDescriptorMappingSourceDataEXT { pushDataOffset: 0 },
            ),
        ]
    }
}

impl Drop for Bindless {
    fn drop(&mut self) {
        let destroy = |heap: &DescriptorHeap| {
            // SAFETY: the device drops the heaps once, before itself, with nothing using them.
            unsafe {
                (self.destroy_buffer)(self.fns_device, heap.buffer, ptr::null());
                (self.free_memory)(self.fns_device, heap.memory, ptr::null());
            }
        };
        destroy(&self.resources);
        destroy(&self.samplers);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Table {
    Resources,
    Samplers,
}

/// A slot in one of the heaps, freed on drop: the backend's `ShaderView` and `Sampler`.
pub(crate) struct Descriptor {
    shared: Arc<Shared>,
    index: u32,
    table: Table,
}

impl Descriptor {
    /// Takes a free slot of `table`; write it with [`write_resource`](Self::write_resource) or
    /// [`write_sampler`](Self::write_sampler).
    pub(crate) fn new(shared: &Arc<Shared>, table: Table) -> Result<Self> {
        let heap = match table {
            Table::Resources => &shared.bindless.resources,
            Table::Samplers => &shared.bindless.samplers,
        };
        Ok(Self {
            index: heap.take()?,
            shared: shared.clone(),
            table,
        })
    }

    pub(crate) fn index(&self) -> u32 {
        self.index
    }

    pub(crate) fn table(&self) -> Table {
        self.table
    }

    /// Writes a resource descriptor (buffer range, image view, acceleration structure) into
    /// the slot.
    pub(crate) fn write_resource(&self, info: &VkResourceDescriptorInfoEXT) -> Result<()> {
        debug_assert_eq!(self.table, Table::Resources);
        let range = self.shared.bindless.resources.slot(self.index);
        let s = &self.shared;
        // SAFETY: a valid descriptor info and a host range of this slot, which `self` owns.
        check(unsafe { (s.fns.vkWriteResourceDescriptorsEXT)(s.raw, 1, info, &range) })
    }

    /// Writes a sampler into the slot.
    pub(crate) fn write_sampler(&self, info: &VkSamplerCreateInfo) -> Result<()> {
        debug_assert_eq!(self.table, Table::Samplers);
        let range = self.shared.bindless.samplers.slot(self.index);
        let s = &self.shared;
        // SAFETY: as above.
        check(unsafe { (s.fns.vkWriteSamplerDescriptorsEXT)(s.raw, 1, info, &range) })
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        match self.table {
            Table::Resources => self.shared.bindless.resources.free(self.index),
            Table::Samplers => self.shared.bindless.samplers.free(self.index),
        }
    }
}
