//! Buffers, textures, views and samplers; placed and sparse resources; format support.
//!
//! Every resource is `VK_SHARING_MODE_CONCURRENT` over the queue families in use (no ownership
//! transfers, 0016 §5). Buffers always have device addresses: buffer descriptors are address
//! ranges.

use core::ptr::{self, NonNull};

use alloc::sync::Arc;

use super::descriptor::{Descriptor, Table};
use super::device::{Device, Shared};
use super::ffi::*;
use super::memory::{Dedicated, Heap, Memory};
use super::queue::Queue;
use super::unsupported;
use crate::{
    BufferDesc, BufferUsage, BufferViewDesc, BufferViewKind, DepthStencilViewDesc, Format,
    FormatSupport, MemoryRequirements, RenderTargetViewDesc, Result, SamplerDesc, SparseBind,
    SparseTextureLayout, TextureDesc, TextureViewDesc,
};

// --- Buffers ---

pub(crate) struct BufferInner {
    shared: Arc<Shared>,
    raw: VkBuffer,
    address: u64,
    mapped: Option<NonNull<u8>>,
    /// Committed memory, or the heap a placed buffer lives in; `None` for sparse and wrapped
    /// buffers.
    _memory: Option<Arc<Memory>>,
    /// Destroy `raw` on drop (false for buffers wrapped from native ones).
    owned: bool,
}

// SAFETY: the mapping is only read and written through the public API's unsafe accessors; Vulkan
// buffers may be used from any thread.
unsafe impl Send for BufferInner {}
// SAFETY: as above.
unsafe impl Sync for BufferInner {}

impl Drop for BufferInner {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: no pending submission uses the buffer (the public API's contract).
            unsafe { (self.shared.fns.vkDestroyBuffer)(self.shared.raw, self.raw, ptr::null()) };
        }
    }
}

/// A buffer; clones share it (an acceleration structure keeps its storage buffer this way).
#[derive(Clone)]
pub(crate) struct Buffer(Arc<BufferInner>);

impl Buffer {
    pub(crate) fn raw(&self) -> VkBuffer {
        self.0.raw
    }

    pub(crate) fn address(&self) -> u64 {
        self.0.address
    }

    pub(crate) fn mapped(&self) -> Option<NonNull<u8>> {
        self.0.mapped
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.0.shared
    }

    /// Wraps a bound buffer of this device.
    pub(crate) fn from_parts(
        shared: &Arc<Shared>,
        raw: VkBuffer,
        mapped: Option<NonNull<u8>>,
        memory: Option<Arc<Memory>>,
        owned: bool,
    ) -> Self {
        let info = VkBufferDeviceAddressInfo {
            buffer: raw,
            ..Default::default()
        };
        // SAFETY: a bound buffer created with device address usage.
        let address = unsafe { (shared.fns.vkGetBufferDeviceAddress)(shared.raw, &info) };
        Self(Arc::new(BufferInner {
            shared: shared.clone(),
            raw,
            address,
            mapped,
            _memory: memory,
            owned,
        }))
    }
}

/// `BufferUsage` as Vulkan usage flags. Copies, fills and device addresses are always allowed.
pub(crate) fn buffer_usage(shared: &Shared, usage: BufferUsage) -> VkBufferUsageFlags2 {
    let mut flags = VK_BUFFER_USAGE_2_TRANSFER_SRC_BIT
        | VK_BUFFER_USAGE_2_TRANSFER_DST_BIT
        | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT;
    if usage.contains(BufferUsage::INDEX) {
        flags |= VK_BUFFER_USAGE_2_INDEX_BUFFER_BIT;
    }
    if usage.contains(BufferUsage::UNIFORM) {
        flags |= VK_BUFFER_USAGE_2_UNIFORM_BUFFER_BIT;
    }
    if usage.intersects(BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE) {
        flags |= VK_BUFFER_USAGE_2_STORAGE_BUFFER_BIT;
    }
    if usage.contains(BufferUsage::INDIRECT) {
        flags |= VK_BUFFER_USAGE_2_INDIRECT_BUFFER_BIT;
        if shared.ext.conditional_rendering {
            flags |= VK_BUFFER_USAGE_2_CONDITIONAL_RENDERING_BIT_EXT;
        }
    }
    if shared.ext.acceleration_structure {
        if usage.contains(BufferUsage::ACCELERATION_STRUCTURE_INPUT) {
            flags |= VK_BUFFER_USAGE_2_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR;
        }
        if usage.contains(BufferUsage::ACCELERATION_STRUCTURE_STORAGE) {
            flags |= VK_BUFFER_USAGE_2_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR;
        }
    }
    flags
}

/// Creates an unbound `VkBuffer` for `desc` (`flags`: sparse creation flags).
pub(crate) fn create_raw_buffer(
    shared: &Shared,
    desc: &BufferDesc,
    flags: VkBufferCreateFlags,
) -> Result<VkBuffer> {
    let usage = VkBufferUsageFlags2CreateInfo {
        usage: buffer_usage(shared, desc.usage),
        ..Default::default()
    };
    let (sharing, family_count, families) = shared.families.sharing();
    let info = VkBufferCreateInfo {
        pNext: (&raw const usage).cast(),
        flags,
        size: desc.size,
        sharingMode: sharing,
        queueFamilyIndexCount: family_count,
        pQueueFamilyIndices: families,
        ..Default::default()
    };
    let mut raw = 0;
    // SAFETY: a valid create info chain and out pointer.
    let r = unsafe { (shared.fns.vkCreateBuffer)(shared.raw, &info, ptr::null(), &mut raw) };
    shared.lost.result(r)?;
    Ok(raw)
}

fn bind_buffer(shared: &Shared, raw: VkBuffer, memory: VkDeviceMemory, offset: u64) -> Result<()> {
    let bind = VkBindBufferMemoryInfo {
        buffer: raw,
        memory,
        memoryOffset: offset,
        ..Default::default()
    };
    // SAFETY: an unbound buffer and memory of a type it accepts, at an aligned offset.
    shared
        .lost
        .result(unsafe { (shared.fns.vkBindBufferMemory2)(shared.raw, 1, &bind) })
}

fn destroy_buffer(shared: &Shared, raw: VkBuffer) {
    // SAFETY: a buffer nothing uses yet.
    unsafe { (shared.fns.vkDestroyBuffer)(shared.raw, raw, ptr::null()) };
}

pub(crate) fn create_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_raw_buffer(s, desc, 0)?;
    let info = VkBufferMemoryRequirementsInfo2 {
        buffer: raw,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: a live buffer and valid structures.
    unsafe { (s.fns.vkGetBufferMemoryRequirements2)(s.raw, &info, &mut req) };
    let r = req.memoryRequirements;
    let Some(memory_type) = s.memory.find_type(r.memoryTypeBits, desc.memory) else {
        destroy_buffer(s, raw);
        return Err(unsupported());
    };
    let memory = match Memory::allocate(s, r.size, memory_type, Dedicated::Buffer(raw), ptr::null())
    {
        Ok(m) => m,
        Err(e) => {
            destroy_buffer(s, raw);
            return Err(e);
        }
    };
    if let Err(e) = bind_buffer(s, raw, memory.raw, 0) {
        destroy_buffer(s, raw);
        return Err(e);
    }
    let mapped = memory.mapped;
    Ok(Buffer::from_parts(
        s,
        raw,
        mapped,
        Some(Arc::new(memory)),
        true,
    ))
}

pub(crate) fn buffer_requirements(device: &Device, desc: &BufferDesc) -> MemoryRequirements {
    let s = &device.shared;
    let usage = VkBufferUsageFlags2CreateInfo {
        usage: buffer_usage(s, desc.usage),
        ..Default::default()
    };
    let (sharing, family_count, families) = s.families.sharing();
    let create = VkBufferCreateInfo {
        pNext: (&raw const usage).cast(),
        size: desc.size,
        sharingMode: sharing,
        queueFamilyIndexCount: family_count,
        pQueueFamilyIndices: families,
        ..Default::default()
    };
    let info = VkDeviceBufferMemoryRequirements {
        pCreateInfo: &create,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: valid structures.
    unsafe { (s.fns.vkGetDeviceBufferMemoryRequirements)(s.raw, &info, &mut req) };
    MemoryRequirements {
        size: req.memoryRequirements.size,
        alignment: req.memoryRequirements.alignment,
    }
}

pub(crate) fn create_placed_buffer(
    device: &Device,
    heap: &Heap,
    offset: u64,
    desc: &BufferDesc,
) -> Result<Buffer> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_raw_buffer(s, desc, 0)?;
    let info = VkBufferMemoryRequirementsInfo2 {
        buffer: raw,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: a live buffer and valid structures.
    unsafe { (s.fns.vkGetBufferMemoryRequirements2)(s.raw, &info, &mut req) };
    let memory = &heap.memory;
    if req.memoryRequirements.memoryTypeBits & (1 << memory.memory_type) == 0 {
        destroy_buffer(s, raw);
        return Err(unsupported());
    }
    if let Err(e) = bind_buffer(s, raw, memory.raw, offset) {
        destroy_buffer(s, raw);
        return Err(e);
    }
    // SAFETY: `api.rs` checked that the buffer lies within the heap.
    let mapped = memory.mapped.map(|m| unsafe { m.add(offset as usize) });
    Ok(Buffer::from_parts(
        s,
        raw,
        mapped,
        Some(memory.clone()),
        true,
    ))
}

pub(crate) fn create_buffer_view(
    device: &Device,
    buffer: &crate::Buffer,
    desc: &BufferViewDesc,
) -> Result<Descriptor> {
    let s = &device.shared;
    let range = VkDeviceAddressRangeKHR {
        address: buffer.raw.address() + desc.offset,
        size: desc.size,
    };
    let info = VkResourceDescriptorInfoEXT {
        r#type: match desc.kind {
            BufferViewKind::Uniform => VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
            BufferViewKind::Structured { .. } | BufferViewKind::Raw => {
                VK_DESCRIPTOR_TYPE_STORAGE_BUFFER
            }
        },
        data: VkResourceDescriptorDataEXT {
            pAddressRange: &range,
        },
        ..Default::default()
    };
    let view = Descriptor::new(s, Table::Resources)?;
    view.write_resource(&info)?;
    Ok(view)
}

// --- Textures (resource area) ---

pub(crate) struct TextureInner {
    shared: Arc<Shared>,
    image: VkImage,
    aspect: VkImageAspectFlags,
    depth: bool,
}

/// A texture; clones share it.
#[derive(Clone)]
pub(crate) struct Texture(Arc<TextureInner>);

impl Texture {
    pub(crate) fn image(&self) -> VkImage {
        self.0.image
    }

    /// Every aspect of the format (color, or depth and stencil).
    pub(crate) fn aspect(&self) -> VkImageAspectFlags {
        self.0.aspect
    }

    /// A depth (or depth-stencil) format: its shader-read layout is the depth read-only one.
    pub(crate) fn is_depth(&self) -> bool {
        self.0.depth
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.0.shared
    }
}

pub(crate) struct RenderTargetView {
    _texture: Texture,
}

pub(crate) struct DepthStencilView {
    _texture: Texture,
}

pub(crate) fn format_support(device: &Device, format: Format) -> FormatSupport {
    let _ = (device, format);
    todo!("vulkan: format_support")
}

pub(crate) fn create_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    let _ = (device, desc);
    todo!("vulkan: create_texture")
}

pub(crate) fn texture_requirements(device: &Device, desc: &TextureDesc) -> MemoryRequirements {
    let _ = (device, desc);
    todo!("vulkan: texture_requirements")
}

pub(crate) fn create_placed_texture(
    device: &Device,
    heap: &Heap,
    offset: u64,
    desc: &TextureDesc,
) -> Result<Texture> {
    let _ = (device, heap, offset, desc);
    todo!("vulkan: create_placed_texture")
}

pub(crate) fn create_render_target_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &RenderTargetViewDesc,
) -> Result<RenderTargetView> {
    let _ = (device, texture, desc);
    todo!("vulkan: create_render_target_view")
}

pub(crate) fn create_depth_stencil_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &DepthStencilViewDesc,
) -> Result<DepthStencilView> {
    let _ = (device, texture, desc);
    todo!("vulkan: create_depth_stencil_view")
}

pub(crate) fn create_texture_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &TextureViewDesc,
) -> Result<Descriptor> {
    let _ = (device, texture, desc);
    todo!("vulkan: create_texture_view")
}

pub(crate) fn create_sampler(device: &Device, desc: &SamplerDesc) -> Result<Descriptor> {
    let _ = (device, desc);
    todo!("vulkan: create_sampler")
}

// --- Sparse resources (resource area) ---

pub(crate) fn create_sparse_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    let _ = (device, desc);
    todo!("vulkan: create_sparse_buffer")
}

pub(crate) fn create_sparse_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    let _ = (device, desc);
    todo!("vulkan: create_sparse_texture")
}

pub(crate) fn sparse_texture_layout(texture: &Texture) -> SparseTextureLayout {
    let _ = texture;
    todo!("vulkan: sparse_texture_layout")
}

/// # Safety
/// As `contract::Queue::bind_sparse`.
pub(crate) unsafe fn bind_sparse(queue: &Queue, binds: &[SparseBind<'_>]) -> Result<()> {
    let _ = (queue, binds);
    todo!("vulkan: bind_sparse")
}
