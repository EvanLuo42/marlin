//! Buffers, textures, views and samplers; placed and sparse resources; format support.
//!
//! Every resource is `VK_SHARING_MODE_CONCURRENT` over the queue families in use (no ownership
//! transfers, 0016 §5). Buffers always have device addresses: buffer descriptors are address
//! ranges.

use core::ffi::c_void;
use core::ptr::{self, NonNull};

use alloc::sync::Arc;

use super::descriptor::{Descriptor, Table};
use super::device::{Device, Shared};
use super::ffi::*;
use super::memory::{Dedicated, Heap, Memory};
use super::queue::Queue;
use super::share::External;
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
    memory: Option<Arc<Memory>>,
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

    /// The memory it is bound to: its own for committed buffers, the heap's for placed ones.
    pub(crate) fn memory(&self) -> Option<&Memory> {
        self.0.memory.as_deref()
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
            memory,
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
    create_raw_buffer_with(shared, desc, flags, ptr::null())
}

/// As [`create_raw_buffer`], with `next` chained into the create info (external memory).
pub(crate) fn create_raw_buffer_with(
    shared: &Shared,
    desc: &BufferDesc,
    flags: VkBufferCreateFlags,
    next: *const c_void,
) -> Result<VkBuffer> {
    let usage = VkBufferUsageFlags2CreateInfo {
        usage: buffer_usage(shared, desc.usage),
        pNext: next,
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
    let external = External::export(&device.shared, desc.usage.contains(BufferUsage::SHARED));
    create_buffer_with(device, desc, external.as_ref())
}

/// A committed buffer; `external` makes its memory exportable, or imports it (share.rs).
pub(crate) fn create_buffer_with(
    device: &Device,
    desc: &BufferDesc,
    external: Option<&External>,
) -> Result<Buffer> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_raw_buffer_with(
        s,
        desc,
        0,
        external.map_or(ptr::null(), |e| e.buffer_info()),
    )?;
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
    let next = external.map_or(ptr::null(), |e| e.memory_info());
    let memory = match Memory::allocate(s, r.size, memory_type, Dedicated::Buffer(raw), next) {
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

use alloc::vec::Vec;

use marlin_sync::Mutex;

use super::command::layout;
use crate::{
    AddressMode, BorderColor, CompareOp, Filter, FormatFeatures, Layout, MemoryLocation,
    SPARSE_TILE_SIZE, SamplerReduction, SparseTextureRegion, TextureDimension, TextureUsage,
    TextureViewDimension,
};

/// `format` as a `VkFormat`. `Rgb10a2*` is Vulkan's `A2B10G10R10` (red in the low bits, as in
/// D3D's `R10G10B10A2`), `Rg11b10Float` is `B10G11R11`, and BC1 keeps its 1-bit alpha (`RGBA`).
/// Every format has one; whether the device takes it is [`format_support`]'s business.
pub(crate) const fn vk_format(format: Format) -> VkFormat {
    use Format::*;
    match format {
        R8Unorm => VK_FORMAT_R8_UNORM,
        R8Snorm => VK_FORMAT_R8_SNORM,
        R8Uint => VK_FORMAT_R8_UINT,
        R8Sint => VK_FORMAT_R8_SINT,
        Rg8Unorm => VK_FORMAT_R8G8_UNORM,
        Rg8Snorm => VK_FORMAT_R8G8_SNORM,
        Rg8Uint => VK_FORMAT_R8G8_UINT,
        Rg8Sint => VK_FORMAT_R8G8_SINT,
        Rgba8Unorm => VK_FORMAT_R8G8B8A8_UNORM,
        Rgba8UnormSrgb => VK_FORMAT_R8G8B8A8_SRGB,
        Rgba8Snorm => VK_FORMAT_R8G8B8A8_SNORM,
        Rgba8Uint => VK_FORMAT_R8G8B8A8_UINT,
        Rgba8Sint => VK_FORMAT_R8G8B8A8_SINT,
        Bgra8Unorm => VK_FORMAT_B8G8R8A8_UNORM,
        Bgra8UnormSrgb => VK_FORMAT_B8G8R8A8_SRGB,
        Rgb10a2Unorm => VK_FORMAT_A2B10G10R10_UNORM_PACK32,
        Rgb10a2Uint => VK_FORMAT_A2B10G10R10_UINT_PACK32,
        Rg11b10Float => VK_FORMAT_B10G11R11_UFLOAT_PACK32,
        Rgb9e5Ufloat => VK_FORMAT_E5B9G9R9_UFLOAT_PACK32,
        R16Unorm => VK_FORMAT_R16_UNORM,
        R16Snorm => VK_FORMAT_R16_SNORM,
        R16Uint => VK_FORMAT_R16_UINT,
        R16Sint => VK_FORMAT_R16_SINT,
        R16Float => VK_FORMAT_R16_SFLOAT,
        Rg16Unorm => VK_FORMAT_R16G16_UNORM,
        Rg16Snorm => VK_FORMAT_R16G16_SNORM,
        Rg16Uint => VK_FORMAT_R16G16_UINT,
        Rg16Sint => VK_FORMAT_R16G16_SINT,
        Rg16Float => VK_FORMAT_R16G16_SFLOAT,
        Rgba16Unorm => VK_FORMAT_R16G16B16A16_UNORM,
        Rgba16Snorm => VK_FORMAT_R16G16B16A16_SNORM,
        Rgba16Uint => VK_FORMAT_R16G16B16A16_UINT,
        Rgba16Sint => VK_FORMAT_R16G16B16A16_SINT,
        Rgba16Float => VK_FORMAT_R16G16B16A16_SFLOAT,
        R32Uint => VK_FORMAT_R32_UINT,
        R32Sint => VK_FORMAT_R32_SINT,
        R32Float => VK_FORMAT_R32_SFLOAT,
        Rg32Uint => VK_FORMAT_R32G32_UINT,
        Rg32Sint => VK_FORMAT_R32G32_SINT,
        Rg32Float => VK_FORMAT_R32G32_SFLOAT,
        Rgba32Uint => VK_FORMAT_R32G32B32A32_UINT,
        Rgba32Sint => VK_FORMAT_R32G32B32A32_SINT,
        Rgba32Float => VK_FORMAT_R32G32B32A32_SFLOAT,
        D16Unorm => VK_FORMAT_D16_UNORM,
        D32Float => VK_FORMAT_D32_SFLOAT,
        D32FloatS8 => VK_FORMAT_D32_SFLOAT_S8_UINT,
        Bc1Unorm => VK_FORMAT_BC1_RGBA_UNORM_BLOCK,
        Bc1UnormSrgb => VK_FORMAT_BC1_RGBA_SRGB_BLOCK,
        Bc2Unorm => VK_FORMAT_BC2_UNORM_BLOCK,
        Bc2UnormSrgb => VK_FORMAT_BC2_SRGB_BLOCK,
        Bc3Unorm => VK_FORMAT_BC3_UNORM_BLOCK,
        Bc3UnormSrgb => VK_FORMAT_BC3_SRGB_BLOCK,
        Bc4Unorm => VK_FORMAT_BC4_UNORM_BLOCK,
        Bc4Snorm => VK_FORMAT_BC4_SNORM_BLOCK,
        Bc5Unorm => VK_FORMAT_BC5_UNORM_BLOCK,
        Bc5Snorm => VK_FORMAT_BC5_SNORM_BLOCK,
        Bc6hUfloat => VK_FORMAT_BC6H_UFLOAT_BLOCK,
        Bc6hSfloat => VK_FORMAT_BC6H_SFLOAT_BLOCK,
        Bc7Unorm => VK_FORMAT_BC7_UNORM_BLOCK,
        Bc7UnormSrgb => VK_FORMAT_BC7_SRGB_BLOCK,
        Etc2Rgb8Unorm => VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK,
        Etc2Rgb8UnormSrgb => VK_FORMAT_ETC2_R8G8B8_SRGB_BLOCK,
        Etc2Rgb8A1Unorm => VK_FORMAT_ETC2_R8G8B8A1_UNORM_BLOCK,
        Etc2Rgb8A1UnormSrgb => VK_FORMAT_ETC2_R8G8B8A1_SRGB_BLOCK,
        Etc2Rgba8Unorm => VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK,
        Etc2Rgba8UnormSrgb => VK_FORMAT_ETC2_R8G8B8A8_SRGB_BLOCK,
        EacR11Unorm => VK_FORMAT_EAC_R11_UNORM_BLOCK,
        EacR11Snorm => VK_FORMAT_EAC_R11_SNORM_BLOCK,
        EacRg11Unorm => VK_FORMAT_EAC_R11G11_UNORM_BLOCK,
        EacRg11Snorm => VK_FORMAT_EAC_R11G11_SNORM_BLOCK,
        Astc4x4Unorm => VK_FORMAT_ASTC_4x4_UNORM_BLOCK,
        Astc4x4UnormSrgb => VK_FORMAT_ASTC_4x4_SRGB_BLOCK,
        Astc5x5Unorm => VK_FORMAT_ASTC_5x5_UNORM_BLOCK,
        Astc5x5UnormSrgb => VK_FORMAT_ASTC_5x5_SRGB_BLOCK,
        Astc6x6Unorm => VK_FORMAT_ASTC_6x6_UNORM_BLOCK,
        Astc6x6UnormSrgb => VK_FORMAT_ASTC_6x6_SRGB_BLOCK,
        Astc8x8Unorm => VK_FORMAT_ASTC_8x8_UNORM_BLOCK,
        Astc8x8UnormSrgb => VK_FORMAT_ASTC_8x8_SRGB_BLOCK,
        Astc10x10Unorm => VK_FORMAT_ASTC_10x10_UNORM_BLOCK,
        Astc10x10UnormSrgb => VK_FORMAT_ASTC_10x10_SRGB_BLOCK,
        Astc12x12Unorm => VK_FORMAT_ASTC_12x12_UNORM_BLOCK,
        Astc12x12UnormSrgb => VK_FORMAT_ASTC_12x12_SRGB_BLOCK,
    }
}

/// Every aspect of `format`: color, or depth (and stencil).
pub(crate) const fn aspect_of(format: Format) -> VkImageAspectFlags {
    match format {
        Format::D32FloatS8 => VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT,
        Format::D16Unorm | Format::D32Float => VK_IMAGE_ASPECT_DEPTH_BIT,
        _ => VK_IMAGE_ASPECT_COLOR_BIT,
    }
}

/// The optimal-tiling features of `format` on the device.
fn optimal_features(shared: &Shared, format: VkFormat) -> VkFormatFeatureFlags2 {
    let mut p3 = VkFormatProperties3::default();
    let mut p2 = VkFormatProperties2 {
        pNext: (&raw mut p3).cast(),
        ..Default::default()
    };
    // SAFETY: a physical device of this instance and a valid output chain.
    unsafe {
        (shared.instance.fns.vkGetPhysicalDeviceFormatProperties2)(shared.physical, format, &mut p2)
    };
    p3.optimalTilingFeatures
}

/// `vkGetPhysicalDeviceImageFormatProperties2` for a 2D optimal image; `None` if the device
/// cannot make it.
fn image_format_properties(
    shared: &Shared,
    format: VkFormat,
    usage: VkImageUsageFlags,
    flags: VkImageCreateFlags,
) -> Option<VkImageFormatProperties> {
    let info = VkPhysicalDeviceImageFormatInfo2 {
        format,
        r#type: VK_IMAGE_TYPE_2D,
        tiling: VK_IMAGE_TILING_OPTIMAL,
        usage,
        flags,
        ..Default::default()
    };
    let mut props = VkImageFormatProperties2::default();
    // SAFETY: valid structures.
    let r = unsafe {
        (shared
            .instance
            .fns
            .vkGetPhysicalDeviceImageFormatProperties2)(shared.physical, &info, &mut props)
    };
    (r == VK_SUCCESS).then_some(props.imageFormatProperties)
}

/// `sampleCounts` (Vulkan's bit `n` is `n` samples) as the API's mask (bit `n` set for `n`
/// samples).
fn sample_mask(counts: VkSampleCountFlags) -> u32 {
    [1, 2, 4, 8, 16]
        .into_iter()
        .filter(|&c| counts & c != 0)
        .fold(0, |mask, c| mask | 1 << c)
}

/// A format's sparse tile shape on the device: 2D, single-sampled, standard 64 KiB blocks.
/// `None` if it has no sparse support of that kind.
fn sparse_granularity(shared: &Shared, format: Format) -> Option<VkExtent3D> {
    let info = VkPhysicalDeviceSparseImageFormatInfo2 {
        format: vk_format(format),
        r#type: VK_IMAGE_TYPE_2D,
        samples: VK_SAMPLE_COUNT_1_BIT as VkSampleCountFlagBits,
        usage: VK_IMAGE_USAGE_SAMPLED_BIT,
        tiling: VK_IMAGE_TILING_OPTIMAL,
        ..Default::default()
    };
    let f = &shared.instance.fns;
    let mut count = 0;
    // SAFETY: the two-call idiom with valid pointers.
    unsafe {
        (f.vkGetPhysicalDeviceSparseImageFormatProperties2)(
            shared.physical,
            &info,
            &mut count,
            ptr::null_mut(),
        )
    };
    let mut props = alloc::vec![VkSparseImageFormatProperties2::default(); count as usize];
    // SAFETY: as above; `props` holds `count` entries.
    unsafe {
        (f.vkGetPhysicalDeviceSparseImageFormatProperties2)(
            shared.physical,
            &info,
            &mut count,
            props.as_mut_ptr(),
        )
    };
    let p = props.iter().map(|p| p.properties).find(|p| {
        p.aspectMask == VK_IMAGE_ASPECT_COLOR_BIT
            && p.flags & VK_SPARSE_IMAGE_FORMAT_NONSTANDARD_BLOCK_SIZE_BIT == 0
    })?;
    let g = p.imageGranularity;
    let block = format.block_extent();
    let bytes = (g.width / block) as u64
        * (g.height / block) as u64
        * g.depth as u64
        * format.block_size() as u64;
    (bytes == SPARSE_TILE_SIZE).then_some(g)
}

pub(crate) fn format_support(device: &Device, format: Format) -> FormatSupport {
    let s = &device.shared;
    let vk = vk_format(format);
    let f = optimal_features(s, vk);
    let has = |bit: VkFormatFeatureFlags2| f & bit == bit;
    let any_use = VK_FORMAT_FEATURE_2_SAMPLED_IMAGE_BIT
        | VK_FORMAT_FEATURE_2_STORAGE_IMAGE_BIT
        | VK_FORMAT_FEATURE_2_COLOR_ATTACHMENT_BIT
        | VK_FORMAT_FEATURE_2_DEPTH_STENCIL_ATTACHMENT_BIT;
    if f & any_use == 0 {
        return FormatSupport::default();
    }
    // The shared-exponent format is read-only in the API (as on D3D12), whatever the driver says.
    let read_only = format == Format::Rgb9e5Ufloat;
    let mut features = FormatFeatures::default();
    let mut set = |condition: bool, flag| {
        if condition {
            features |= flag;
        }
    };
    let sampled = has(VK_FORMAT_FEATURE_2_SAMPLED_IMAGE_BIT);
    let storage = has(VK_FORMAT_FEATURE_2_STORAGE_IMAGE_BIT) && !read_only;
    let target = has(VK_FORMAT_FEATURE_2_COLOR_ATTACHMENT_BIT) && !read_only;
    let depth = has(VK_FORMAT_FEATURE_2_DEPTH_STENCIL_ATTACHMENT_BIT);
    set(sampled, FormatFeatures::SAMPLED);
    set(
        sampled && has(VK_FORMAT_FEATURE_2_SAMPLED_IMAGE_FILTER_LINEAR_BIT) && !format.is_integer(),
        FormatFeatures::FILTER,
    );
    set(
        sampled && has(VK_FORMAT_FEATURE_2_SAMPLED_IMAGE_DEPTH_COMPARISON_BIT),
        FormatFeatures::COMPARISON,
    );
    // Shaders declare writable textures without a format (`shaderStorageImage*WithoutFormat`).
    set(
        storage && has(VK_FORMAT_FEATURE_2_STORAGE_WRITE_WITHOUT_FORMAT_BIT),
        FormatFeatures::STORAGE_WRITE,
    );
    set(
        storage && has(VK_FORMAT_FEATURE_2_STORAGE_READ_WITHOUT_FORMAT_BIT),
        FormatFeatures::STORAGE_READ,
    );
    set(
        storage && has(VK_FORMAT_FEATURE_2_STORAGE_IMAGE_ATOMIC_BIT),
        FormatFeatures::STORAGE_ATOMIC,
    );
    set(target, FormatFeatures::RENDER_TARGET);
    set(
        target && has(VK_FORMAT_FEATURE_2_COLOR_ATTACHMENT_BLEND_BIT),
        FormatFeatures::BLEND,
    );
    set(depth, FormatFeatures::DEPTH_STENCIL);
    let mut sample_counts = 0;
    if target || depth {
        let usage = match depth {
            true => VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,
            false => VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
        };
        sample_counts = image_format_properties(s, vk, usage, 0)
            .map_or(1 << 1, |p| sample_mask(p.sampleCounts) | 1 << 1);
        if sampled {
            let read = image_format_properties(s, vk, usage | VK_IMAGE_USAGE_SAMPLED_BIT, 0);
            set(
                read.is_some_and(|p| p.sampleCounts > VK_SAMPLE_COUNT_1_BIT),
                FormatFeatures::MULTISAMPLE_LOAD,
            );
        }
    }
    // `vkCmdResolveImage2`: a color attachment format that is copied both ways and multisamples;
    // integer formats do not average.
    let transfers = VK_FORMAT_FEATURE_2_TRANSFER_SRC_BIT | VK_FORMAT_FEATURE_2_TRANSFER_DST_BIT;
    set(
        target && has(transfers) && !format.is_integer() && sample_counts > 1 << 1,
        FormatFeatures::RESOLVE,
    );
    let sparse = s.features.sparse_resources
        && !format.is_depth()
        && sparse_granularity(s, format).is_some()
        && image_format_properties(
            s,
            vk,
            VK_IMAGE_USAGE_SAMPLED_BIT,
            VK_IMAGE_CREATE_SPARSE_BINDING_BIT | VK_IMAGE_CREATE_SPARSE_RESIDENCY_BIT,
        )
        .is_some();
    set(sampled && sparse, FormatFeatures::SPARSE);
    FormatSupport {
        features,
        sample_counts,
    }
}

/// `desc.usage` as Vulkan usage. Copies go both ways wherever the format copies: storage clears
/// (`vkCmdClearColorImage`) and resolves are transfers in Vulkan, and the API names no copy usage
/// for them.
fn image_usage(shared: &Shared, desc: &TextureDesc) -> VkImageUsageFlags {
    let features = optimal_features(shared, vk_format(desc.format));
    let mut usage = 0;
    if features & VK_FORMAT_FEATURE_2_TRANSFER_SRC_BIT != 0 {
        usage |= VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    }
    if features & VK_FORMAT_FEATURE_2_TRANSFER_DST_BIT != 0 {
        usage |= VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    }
    let u = desc.usage;
    let mut add = |api: TextureUsage, vk: VkImageUsageFlags| {
        if u.contains(api) {
            usage |= vk;
        }
    };
    add(TextureUsage::COPY_SRC, VK_IMAGE_USAGE_TRANSFER_SRC_BIT);
    add(TextureUsage::COPY_DST, VK_IMAGE_USAGE_TRANSFER_DST_BIT);
    add(TextureUsage::SAMPLED, VK_IMAGE_USAGE_SAMPLED_BIT);
    add(TextureUsage::STORAGE, VK_IMAGE_USAGE_STORAGE_BIT);
    add(
        TextureUsage::RENDER_TARGET,
        VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
    );
    add(
        TextureUsage::DEPTH_STENCIL,
        VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,
    );
    if shared.ext.fragment_shading_rate {
        add(
            TextureUsage::SHADING_RATE,
            VK_IMAGE_USAGE_FRAGMENT_SHADING_RATE_ATTACHMENT_BIT_KHR,
        );
    }
    usage
}

/// Runs `f` with the `VkImageCreateInfo` of `desc` (`flags`: sparse creation flags; `next`: a
/// structure chained last, such as the sharing area's external memory info, or null). Formats
/// with an sRGB twin are mutable between the two (and say so, which keeps their compression);
/// square 2D textures of 6 layers or more may be viewed as cubes.
fn with_image_info<R>(
    shared: &Shared,
    desc: &TextureDesc,
    flags: VkImageCreateFlags,
    next: *const c_void,
    f: impl FnOnce(&VkImageCreateInfo) -> R,
) -> R {
    let linear = desc.format.linear();
    let twin = linear.srgb();
    let formats = [
        vk_format(linear),
        twin.map_or(VK_FORMAT_UNDEFINED, vk_format),
    ];
    let list = VkImageFormatListCreateInfo {
        pNext: next,
        viewFormatCount: 2,
        pViewFormats: formats.as_ptr(),
        ..Default::default()
    };
    let mut flags = flags;
    if twin.is_some() {
        flags |= VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
    }
    let d2 = desc.dimension == TextureDimension::D2;
    if d2 && desc.array_layers >= 6 && desc.width == desc.height && desc.sample_count == 1 {
        flags |= VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT;
    }
    let (sharing, family_count, families) = shared.families.sharing();
    let info = VkImageCreateInfo {
        pNext: match twin {
            Some(_) => (&raw const list).cast(),
            None => next,
        },
        flags,
        imageType: match d2 {
            true => VK_IMAGE_TYPE_2D,
            false => VK_IMAGE_TYPE_3D,
        },
        format: vk_format(desc.format),
        extent: VkExtent3D {
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
        },
        mipLevels: desc.mip_levels,
        arrayLayers: desc.array_layers,
        // `VK_SAMPLE_COUNT_n_BIT` is `n`.
        samples: desc.sample_count as VkSampleCountFlagBits,
        tiling: VK_IMAGE_TILING_OPTIMAL,
        usage: image_usage(shared, desc),
        sharingMode: sharing,
        queueFamilyIndexCount: family_count,
        pQueueFamilyIndices: families,
        initialLayout: VK_IMAGE_LAYOUT_UNDEFINED,
        ..Default::default()
    };
    f(&info)
}

fn create_image(
    shared: &Shared,
    desc: &TextureDesc,
    flags: VkImageCreateFlags,
    next: *const c_void,
) -> Result<VkImage> {
    let mut raw = 0;
    let r = with_image_info(shared, desc, flags, next, |info| {
        // SAFETY: a valid create info chain and out pointer.
        unsafe { (shared.fns.vkCreateImage)(shared.raw, info, ptr::null(), &mut raw) }
    });
    shared.lost.result(r)?;
    Ok(raw)
}

fn destroy_image(shared: &Shared, raw: VkImage) {
    // SAFETY: an image nothing uses yet.
    unsafe { (shared.fns.vkDestroyImage)(shared.raw, raw, ptr::null()) };
}

fn image_requirements(shared: &Shared, image: VkImage) -> VkMemoryRequirements {
    let info = VkImageMemoryRequirementsInfo2 {
        image,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: a live image and valid structures.
    unsafe { (shared.fns.vkGetImageMemoryRequirements2)(shared.raw, &info, &mut req) };
    req.memoryRequirements
}

fn bind_image(shared: &Shared, image: VkImage, memory: VkDeviceMemory, offset: u64) -> Result<()> {
    let bind = VkBindImageMemoryInfo {
        image,
        memory,
        memoryOffset: offset,
        ..Default::default()
    };
    // SAFETY: an unbound image and memory of a type it accepts, at an aligned offset.
    shared
        .lost
        .result(unsafe { (shared.fns.vkBindImageMemory2)(shared.raw, 1, &bind) })
}

/// How a sparse texture's color aspect divides into tiles.
#[derive(Clone, Copy)]
struct SparseImage {
    /// Texels per tile.
    granularity: VkExtent3D,
    /// The first packed mip (the mip count when none are).
    tail_first_lod: u32,
    tail_size: u64,
    tail_offset: u64,
    tail_stride: u64,
    /// One mip tail for every layer (bound with layer 0).
    single_tail: bool,
    size: u64,
    memory_type_bits: u32,
}

pub(crate) struct TextureInner {
    shared: Arc<Shared>,
    image: VkImage,
    aspect: VkImageAspectFlags,
    depth: bool,
    desc: TextureDesc,
    /// Committed memory, or the heap a placed texture lives in; `None` for sparse and wrapped
    /// textures.
    memory: Option<Arc<Memory>>,
    sparse: Option<SparseImage>,
    /// Destroy `image` on drop (false for images wrapped from other owners).
    owned: bool,
    /// Queued to leave UNDEFINED for GENERAL ([`InternalWork`]).
    initial_layout: bool,
    /// The attachment view of a `TextureUsage::SHADING_RATE` texture, else 0.
    shading_rate_view: VkImageView,
}

// SAFETY: Vulkan images may be used from any thread.
unsafe impl Send for TextureInner {}
// SAFETY: as above.
unsafe impl Sync for TextureInner {}

impl Drop for TextureInner {
    fn drop(&mut self) {
        let s = &self.shared;
        if self.shading_rate_view != 0 {
            // SAFETY: no pending submission uses the view (the public API's contract).
            unsafe { (s.fns.vkDestroyImageView)(s.raw, self.shading_rate_view, ptr::null()) };
        }
        if self.owned {
            if self.initial_layout {
                s.internal.forget(s, self.image);
            }
            // SAFETY: no pending submission uses the image (the public API's contract; `forget`
            // waited for the internal one).
            unsafe { (s.fns.vkDestroyImage)(s.raw, self.image, ptr::null()) };
        }
    }
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

    /// The description it was made from.
    pub(crate) fn desc(&self) -> &TextureDesc {
        &self.0.desc
    }

    /// The committed allocation, or the heap of a placed texture; `None` for swapchain, sparse
    /// and wrapped textures.
    pub(crate) fn memory(&self) -> Option<&Memory> {
        self.0.memory.as_deref()
    }

    /// The view `vkCmdBeginRendering` attaches as the shading-rate image (mip 0, layer 0): 0
    /// (`VK_NULL_HANDLE`) unless the texture has `TextureUsage::SHADING_RATE`.
    pub(crate) fn shading_rate_view(&self) -> VkImageView {
        self.0.shading_rate_view
    }

    /// Wraps an image this backend did not create from a `TextureDesc`: swapchain images
    /// (swapchain area; the presentation engine hands them over in `Layout::Present`) and native
    /// images (`texture_from_native`, sharing area; in `Layout::Common`, GENERAL). `owned`:
    /// destroy the image on drop.
    pub(crate) fn from_image(
        shared: &Arc<Shared>,
        image: VkImage,
        desc: &TextureDesc,
        owned: bool,
    ) -> Self {
        Self::from_parts(shared, image, desc, None, owned)
    }

    /// As [`from_image`](Self::from_image), keeping `memory` (the image is bound to it) alive as
    /// long as the texture: imported textures.
    pub(crate) fn from_parts(
        shared: &Arc<Shared>,
        image: VkImage,
        desc: &TextureDesc,
        memory: Option<Arc<Memory>>,
        owned: bool,
    ) -> Self {
        Self(Arc::new(TextureInner {
            shared: shared.clone(),
            image,
            aspect: aspect_of(desc.format),
            depth: desc.format.is_depth(),
            desc: *desc,
            memory,
            sparse: None,
            owned,
            initial_layout: false,
            shading_rate_view: 0,
        }))
    }

    /// A texture this backend created (and owns), in `Layout::Common` before any work using it
    /// runs: queued for [`InternalWork`] unless `initial_layout` is false. Destroys `image` if it
    /// fails.
    fn created(
        shared: &Arc<Shared>,
        image: VkImage,
        desc: &TextureDesc,
        memory: Option<Arc<Memory>>,
        sparse: Option<SparseImage>,
        initial_layout: bool,
    ) -> Result<Self> {
        let aspect = aspect_of(desc.format);
        let mut shading_rate_view = 0;
        if desc.usage.contains(TextureUsage::SHADING_RATE) && shared.ext.fragment_shading_rate {
            let usage = VK_IMAGE_USAGE_FRAGMENT_SHADING_RATE_ATTACHMENT_BIT_KHR;
            let format = vk_format(desc.format);
            match attachment_view(shared, image, format, aspect, 0, 0, usage) {
                Ok(view) => shading_rate_view = view,
                Err(e) => {
                    destroy_image(shared, image);
                    return Err(e);
                }
            }
        }
        if initial_layout {
            shared.internal.add(image, aspect);
        }
        Ok(Self(Arc::new(TextureInner {
            shared: shared.clone(),
            image,
            aspect,
            depth: desc.format.is_depth(),
            desc: *desc,
            memory,
            sparse,
            owned: true,
            initial_layout,
            shading_rate_view,
        })))
    }
}

/// A color attachment for dynamic rendering (draw area reads the fields; resource area creates
/// it; the swapchain area makes one per image with [`render_target_view`]).
pub(crate) struct RenderTargetView {
    pub(crate) texture: Texture,
    pub(crate) view: VkImageView,
    pub(crate) format: VkFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Drop for RenderTargetView {
    fn drop(&mut self) {
        let s = self.texture.shared();
        // SAFETY: no pending submission uses the view (the public API's contract).
        unsafe { (s.fns.vkDestroyImageView)(s.raw, self.view, ptr::null()) };
    }
}

/// A depth attachment for dynamic rendering, as [`RenderTargetView`].
pub(crate) struct DepthStencilView {
    pub(crate) texture: Texture,
    pub(crate) view: VkImageView,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The format has a stencil plane.
    pub(crate) stencil: bool,
    /// Depth tests without writes, the texture also sampled: rendered in
    /// `DEPTH_STENCIL_READ_ONLY_OPTIMAL` (`Layout::DepthStencilRead`).
    pub(crate) read_only: bool,
}

impl Drop for DepthStencilView {
    fn drop(&mut self) {
        let s = self.texture.shared();
        // SAFETY: as for render target views.
        unsafe { (s.fns.vkDestroyImageView)(s.raw, self.view, ptr::null()) };
    }
}

/// The view of one mip and layer of `image` as an attachment of `usage`.
fn attachment_view(
    shared: &Shared,
    image: VkImage,
    format: VkFormat,
    aspect: VkImageAspectFlags,
    mip: u32,
    layer: u32,
    usage: VkImageUsageFlags,
) -> Result<VkImageView> {
    let usage = VkImageViewUsageCreateInfo {
        usage,
        ..Default::default()
    };
    let info = VkImageViewCreateInfo {
        pNext: (&raw const usage).cast(),
        image,
        viewType: VK_IMAGE_VIEW_TYPE_2D,
        format,
        components: identity(),
        subresourceRange: VkImageSubresourceRange {
            aspectMask: aspect,
            baseMipLevel: mip,
            levelCount: 1,
            baseArrayLayer: layer,
            layerCount: 1,
        },
        ..Default::default()
    };
    let mut view = 0;
    // SAFETY: a valid create info chain and out pointer.
    let r = unsafe { (shared.fns.vkCreateImageView)(shared.raw, &info, ptr::null(), &mut view) };
    shared.lost.result(r)?;
    Ok(view)
}

fn identity() -> VkComponentMapping {
    VkComponentMapping {
        r: VK_COMPONENT_SWIZZLE_IDENTITY,
        g: VK_COMPONENT_SWIZZLE_IDENTITY,
        b: VK_COMPONENT_SWIZZLE_IDENTITY,
        a: VK_COMPONENT_SWIZZLE_IDENTITY,
    }
}

/// A render target view of `texture` (whose public description is `desc`), for
/// `create_render_target_view` and swapchain images.
pub(crate) fn render_target_view(
    shared: &Arc<Shared>,
    texture: &Texture,
    desc: &TextureDesc,
    view: &RenderTargetViewDesc,
) -> Result<RenderTargetView> {
    shared.lost.check()?;
    let format = vk_format(view.format.unwrap_or(desc.format));
    let raw = attachment_view(
        shared,
        texture.image(),
        format,
        VK_IMAGE_ASPECT_COLOR_BIT,
        view.mip,
        view.layer,
        VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
    )?;
    let (width, height) = desc.mip_size(view.mip);
    Ok(RenderTargetView {
        texture: texture.clone(),
        view: raw,
        format,
        width,
        height,
    })
}

/// A committed texture; `TextureUsage::SHARED` ones get exportable memory.
pub(crate) fn create_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    let external = External::export(&device.shared, desc.usage.contains(TextureUsage::SHARED));
    let (image_next, memory_next) = external.as_ref().map_or((ptr::null(), ptr::null()), |e| {
        (e.image_info(), e.memory_info())
    });
    create_texture_with(device, desc, image_next, memory_next)
}

/// A committed texture in its own (dedicated) allocation, of the same memory type for the same
/// `desc` every time. `image_next` is chained last onto the `VkImageCreateInfo` and
/// `memory_next` onto the allocation (the sharing area's export structures), or null.
pub(crate) fn create_texture_with(
    device: &Device,
    desc: &TextureDesc,
    image_next: *const c_void,
    memory_next: *const c_void,
) -> Result<Texture> {
    committed_texture(device, desc, image_next, memory_next, true)
}

/// As [`create_texture_with`], over imported memory (`memory_next` holds the import
/// structure): the texture is taken to be in GENERAL already, since a move from UNDEFINED
/// could discard the exporter's contents.
pub(crate) fn import_texture_with(
    device: &Device,
    desc: &TextureDesc,
    image_next: *const c_void,
    memory_next: *const c_void,
) -> Result<Texture> {
    committed_texture(device, desc, image_next, memory_next, false)
}

fn committed_texture(
    device: &Device,
    desc: &TextureDesc,
    image_next: *const c_void,
    memory_next: *const c_void,
    initial_layout: bool,
) -> Result<Texture> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_image(s, desc, 0, image_next)?;
    let r = image_requirements(s, raw);
    let Some(memory_type) = s
        .memory
        .find_type(r.memoryTypeBits, MemoryLocation::GpuOnly)
    else {
        destroy_image(s, raw);
        return Err(unsupported());
    };
    let memory = match Memory::allocate(s, r.size, memory_type, Dedicated::Image(raw), memory_next)
    {
        Ok(m) => m,
        Err(e) => {
            destroy_image(s, raw);
            return Err(e);
        }
    };
    if let Err(e) = bind_image(s, raw, memory.raw, 0) {
        destroy_image(s, raw);
        return Err(e);
    }
    Texture::created(s, raw, desc, Some(Arc::new(memory)), None, initial_layout)
}

pub(crate) fn texture_requirements(device: &Device, desc: &TextureDesc) -> MemoryRequirements {
    let s = &device.shared;
    let mut req = VkMemoryRequirements2::default();
    with_image_info(s, desc, 0, ptr::null(), |create| {
        let info = VkDeviceImageMemoryRequirements {
            pCreateInfo: create,
            ..Default::default()
        };
        // SAFETY: valid structures.
        unsafe { (s.fns.vkGetDeviceImageMemoryRequirements)(s.raw, &info, &mut req) };
    });
    MemoryRequirements {
        size: req.memoryRequirements.size,
        alignment: req.memoryRequirements.alignment,
    }
}

/// A placed texture is not moved to GENERAL on its own: its memory may still hold another
/// resource's live contents, which a layout transition could overwrite. Its first barrier starts
/// from `Layout::Undefined`, as the API asks for memory taken over from another resource.
pub(crate) fn create_placed_texture(
    device: &Device,
    heap: &Heap,
    offset: u64,
    desc: &TextureDesc,
) -> Result<Texture> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_image(s, desc, 0, ptr::null())?;
    let memory = &heap.memory;
    if image_requirements(s, raw).memoryTypeBits & (1 << memory.memory_type) == 0 {
        destroy_image(s, raw);
        return Err(unsupported());
    }
    if let Err(e) = bind_image(s, raw, memory.raw, offset) {
        destroy_image(s, raw);
        return Err(e);
    }
    Texture::created(s, raw, desc, Some(memory.clone()), None, false)
}

pub(crate) fn create_render_target_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &RenderTargetViewDesc,
) -> Result<RenderTargetView> {
    render_target_view(&device.shared, &texture.raw, &texture.desc, desc)
}

pub(crate) fn create_depth_stencil_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &DepthStencilViewDesc,
) -> Result<DepthStencilView> {
    let s = &device.shared;
    s.lost.check()?;
    let t = &texture.desc;
    let format = vk_format(t.format);
    let view = attachment_view(
        s,
        texture.raw.image(),
        format,
        texture.raw.aspect(),
        desc.mip,
        desc.layer,
        VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,
    )?;
    let (width, height) = t.mip_size(desc.mip);
    Ok(DepthStencilView {
        texture: texture.raw.clone(),
        view,
        width,
        height,
        stencil: t.format.has_stencil(),
        read_only: desc.read_only,
    })
}

/// A bindless image descriptor. The view lives in the descriptor (no `VkImageView`); its layout is
/// the one `command::layout` gives shader access of that kind: GENERAL for writable views,
/// `SHADER_READ_ONLY_OPTIMAL` (depth: `DEPTH_STENCIL_READ_ONLY_OPTIMAL`) for read-only ones.
/// Depth planes are views of the depth format with one aspect: `R32Float` / `R16Unorm` the depth,
/// `R8Uint` of [`D32FloatS8`](Format::D32FloatS8) the stencil (read in the red channel).
pub(crate) fn create_texture_view(
    device: &Device,
    texture: &crate::Texture,
    desc: &TextureViewDesc,
) -> Result<Descriptor> {
    let s = &device.shared;
    s.lost.check()?;
    let t = &texture.desc;
    // `api.rs` filled in the format and counts.
    let view_format = desc.format.unwrap_or(t.format);
    let (format, aspect) = match t.format.is_depth() {
        true if view_format == Format::R8Uint => (vk_format(t.format), VK_IMAGE_ASPECT_STENCIL_BIT),
        true => (vk_format(t.format), VK_IMAGE_ASPECT_DEPTH_BIT),
        false => (vk_format(view_format), VK_IMAGE_ASPECT_COLOR_BIT),
    };
    let r = desc.range;
    let usage = VkImageViewUsageCreateInfo {
        usage: match desc.writable {
            true => VK_IMAGE_USAGE_STORAGE_BIT,
            false => VK_IMAGE_USAGE_SAMPLED_BIT,
        },
        ..Default::default()
    };
    let view = VkImageViewCreateInfo {
        pNext: (&raw const usage).cast(),
        image: texture.raw.image(),
        viewType: match desc.dimension {
            TextureViewDimension::D2 => VK_IMAGE_VIEW_TYPE_2D,
            TextureViewDimension::D2Array => VK_IMAGE_VIEW_TYPE_2D_ARRAY,
            TextureViewDimension::D3 => VK_IMAGE_VIEW_TYPE_3D,
            TextureViewDimension::Cube => VK_IMAGE_VIEW_TYPE_CUBE,
            TextureViewDimension::CubeArray => VK_IMAGE_VIEW_TYPE_CUBE_ARRAY,
        },
        format,
        components: identity(),
        subresourceRange: VkImageSubresourceRange {
            aspectMask: aspect,
            baseMipLevel: r.base_mip,
            levelCount: r.mip_count,
            baseArrayLayer: r.base_layer,
            layerCount: r.layer_count,
        },
        ..Default::default()
    };
    let shader_layout = match desc.writable {
        true => Layout::UnorderedAccess,
        false => Layout::ShaderResource,
    };
    let image = VkImageDescriptorInfoEXT {
        pView: &view,
        layout: layout(s, shader_layout, t.format.is_depth()),
        ..Default::default()
    };
    let info = VkResourceDescriptorInfoEXT {
        r#type: match desc.writable {
            true => VK_DESCRIPTOR_TYPE_STORAGE_IMAGE,
            false => VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
        },
        data: VkResourceDescriptorDataEXT { pImage: &image },
        ..Default::default()
    };
    let descriptor = Descriptor::new(s, Table::Resources)?;
    descriptor.write_resource(&info)?;
    Ok(descriptor)
}

fn filter(filter: Filter) -> VkFilter {
    match filter {
        Filter::Nearest => VK_FILTER_NEAREST,
        Filter::Linear => VK_FILTER_LINEAR,
    }
}

fn address_mode(mode: AddressMode) -> VkSamplerAddressMode {
    match mode {
        AddressMode::Repeat => VK_SAMPLER_ADDRESS_MODE_REPEAT,
        AddressMode::MirrorRepeat => VK_SAMPLER_ADDRESS_MODE_MIRRORED_REPEAT,
        AddressMode::ClampToEdge => VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        AddressMode::ClampToBorder => VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_BORDER,
    }
}

/// `op` as a Vulkan compare op (pipelines' depth and stencil tests too).
pub(crate) fn compare_op(op: CompareOp) -> VkCompareOp {
    match op {
        CompareOp::Never => VK_COMPARE_OP_NEVER,
        CompareOp::Less => VK_COMPARE_OP_LESS,
        CompareOp::Equal => VK_COMPARE_OP_EQUAL,
        CompareOp::LessEqual => VK_COMPARE_OP_LESS_OR_EQUAL,
        CompareOp::Greater => VK_COMPARE_OP_GREATER,
        CompareOp::NotEqual => VK_COMPARE_OP_NOT_EQUAL,
        CompareOp::GreaterEqual => VK_COMPARE_OP_GREATER_OR_EQUAL,
        CompareOp::Always => VK_COMPARE_OP_ALWAYS,
    }
}

/// A sampler-heap descriptor. Anisotropy above 1 filters linearly at every level, as D3D's
/// anisotropic filter does; the LOD bias and anisotropy are clamped to the device's limits.
pub(crate) fn create_sampler(device: &Device, desc: &SamplerDesc) -> Result<Descriptor> {
    let s = &device.shared;
    s.lost.check()?;
    let limits = &s.properties.limits;
    let anisotropic = desc.max_anisotropy > 1;
    let pick = |f| match anisotropic {
        true => VK_FILTER_LINEAR,
        false => filter(f),
    };
    let reduction = VkSamplerReductionModeCreateInfo {
        reductionMode: match desc.reduction {
            SamplerReduction::WeightedAverage => VK_SAMPLER_REDUCTION_MODE_WEIGHTED_AVERAGE,
            SamplerReduction::Minimum => VK_SAMPLER_REDUCTION_MODE_MIN,
            SamplerReduction::Maximum => VK_SAMPLER_REDUCTION_MODE_MAX,
        },
        ..Default::default()
    };
    let bias = limits.maxSamplerLodBias;
    let info = VkSamplerCreateInfo {
        pNext: match desc.reduction {
            SamplerReduction::WeightedAverage => ptr::null(),
            _ => (&raw const reduction).cast(),
        },
        magFilter: pick(desc.mag_filter),
        minFilter: pick(desc.min_filter),
        mipmapMode: match (anisotropic, desc.mip_filter) {
            (false, Filter::Nearest) => VK_SAMPLER_MIPMAP_MODE_NEAREST,
            _ => VK_SAMPLER_MIPMAP_MODE_LINEAR,
        },
        addressModeU: address_mode(desc.address_u),
        addressModeV: address_mode(desc.address_v),
        addressModeW: address_mode(desc.address_w),
        mipLodBias: desc.mip_lod_bias.clamp(-bias, bias),
        anisotropyEnable: anisotropic as VkBool32,
        maxAnisotropy: (desc.max_anisotropy as f32).min(limits.maxSamplerAnisotropy),
        compareEnable: desc.compare.is_some() as VkBool32,
        compareOp: desc.compare.map_or(VK_COMPARE_OP_NEVER, compare_op),
        minLod: desc.min_lod,
        maxLod: desc.max_lod,
        borderColor: match desc.border_color {
            BorderColor::TransparentBlack => VK_BORDER_COLOR_FLOAT_TRANSPARENT_BLACK,
            BorderColor::OpaqueBlack => VK_BORDER_COLOR_FLOAT_OPAQUE_BLACK,
            BorderColor::OpaqueWhite => VK_BORDER_COLOR_FLOAT_OPAQUE_WHITE,
        },
        ..Default::default()
    };
    let descriptor = Descriptor::new(s, Table::Samplers)?;
    descriptor.write_sampler(&info)?;
    Ok(descriptor)
}

// --- Sparse resources (resource area) ---

const SPARSE_FLAGS: u32 =
    VK_BUFFER_CREATE_SPARSE_BINDING_BIT | VK_BUFFER_CREATE_SPARSE_RESIDENCY_BIT;

/// Address space without memory. Tiles are bound in [`SPARSE_TILE_SIZE`] steps, so the buffer's
/// sparse block size must divide it.
pub(crate) fn create_sparse_buffer(device: &Device, desc: &BufferDesc) -> Result<Buffer> {
    let s = &device.shared;
    s.lost.check()?;
    let raw = create_raw_buffer(s, desc, SPARSE_FLAGS)?;
    let alignment = buffer_raw_requirements(s, raw).alignment;
    if !SPARSE_TILE_SIZE.is_multiple_of(alignment) {
        destroy_buffer(s, raw);
        return Err(unsupported());
    }
    Ok(Buffer::from_parts(s, raw, None, None, true))
}

fn buffer_raw_requirements(shared: &Shared, buffer: VkBuffer) -> VkMemoryRequirements {
    let info = VkBufferMemoryRequirementsInfo2 {
        buffer,
        ..Default::default()
    };
    let mut req = VkMemoryRequirements2::default();
    // SAFETY: a live buffer and valid structures.
    unsafe { (shared.fns.vkGetBufferMemoryRequirements2)(shared.raw, &info, &mut req) };
    req.memoryRequirements
}

/// A texture of standard 64 KiB tiles (`api.rs` checked `FormatFeatures::SPARSE`). Formats whose
/// sparse layout needs metadata memory are refused. Like committed textures it moves to GENERAL
/// before its first use (layout transitions do not touch unbound tiles).
pub(crate) fn create_sparse_texture(device: &Device, desc: &TextureDesc) -> Result<Texture> {
    let s = &device.shared;
    s.lost.check()?;
    let flags = VK_IMAGE_CREATE_SPARSE_BINDING_BIT | VK_IMAGE_CREATE_SPARSE_RESIDENCY_BIT;
    let raw = create_image(s, desc, flags, ptr::null())?;
    let fail = |e| {
        destroy_image(s, raw);
        Err(e)
    };
    let req = image_requirements(s, raw);
    if req.alignment != SPARSE_TILE_SIZE {
        return fail(unsupported());
    }
    let info = VkImageSparseMemoryRequirementsInfo2 {
        image: raw,
        ..Default::default()
    };
    let mut count = 0;
    // SAFETY: the two-call idiom with valid pointers.
    unsafe {
        (s.fns.vkGetImageSparseMemoryRequirements2)(s.raw, &info, &mut count, ptr::null_mut())
    };
    let mut reqs = alloc::vec![VkSparseImageMemoryRequirements2::default(); count as usize];
    // SAFETY: as above; `reqs` holds `count` entries.
    unsafe {
        (s.fns.vkGetImageSparseMemoryRequirements2)(s.raw, &info, &mut count, reqs.as_mut_ptr())
    };
    reqs.truncate(count as usize);
    let reqs: Vec<VkSparseImageMemoryRequirements> =
        reqs.iter().map(|r| r.memoryRequirements).collect();
    let metadata = reqs
        .iter()
        .any(|r| r.formatProperties.aspectMask & VK_IMAGE_ASPECT_METADATA_BIT != 0);
    let Some(color) = reqs
        .iter()
        .find(|r| r.formatProperties.aspectMask & VK_IMAGE_ASPECT_COLOR_BIT != 0)
    else {
        return fail(unsupported());
    };
    if metadata
        || color.formatProperties.flags & VK_SPARSE_IMAGE_FORMAT_NONSTANDARD_BLOCK_SIZE_BIT != 0
    {
        return fail(unsupported());
    }
    let sparse = SparseImage {
        granularity: color.formatProperties.imageGranularity,
        tail_first_lod: color.imageMipTailFirstLod.min(desc.mip_levels),
        tail_size: color.imageMipTailSize,
        tail_offset: color.imageMipTailOffset,
        tail_stride: color.imageMipTailStride,
        single_tail: color.formatProperties.flags & VK_SPARSE_IMAGE_FORMAT_SINGLE_MIPTAIL_BIT != 0,
        size: req.size,
        memory_type_bits: req.memoryTypeBits,
    };
    Texture::created(s, raw, desc, None, Some(sparse), true)
}

pub(crate) fn sparse_texture_layout(texture: &Texture) -> SparseTextureLayout {
    let sparse = texture.0.sparse.expect("a sparse texture");
    let g = sparse.granularity;
    let packed = sparse.tail_first_lod < texture.desc().mip_levels;
    SparseTextureLayout {
        tile_extent: [g.width, g.height, g.depth],
        tile_count: (sparse.size / SPARSE_TILE_SIZE) as u32,
        standard_mips: sparse.tail_first_lod,
        packed_mip_tiles: match packed {
            true => sparse.tail_size.div_ceil(SPARSE_TILE_SIZE) as u32,
            false => 0,
        },
    }
}

/// `memory` as the memory and byte offset of a bind; `Err` if the resource cannot live in the
/// heap's memory type.
fn bind_memory(
    memory: Option<(&crate::Heap, u32)>,
    memory_type_bits: u32,
) -> Result<(VkDeviceMemory, u64)> {
    match memory {
        None => Ok((0, 0)),
        Some((heap, tile)) => {
            let m = &heap.raw.memory;
            if memory_type_bits & (1 << m.memory_type) == 0 {
                return Err(unsupported());
            }
            Ok((m.raw, tile as u64 * SPARSE_TILE_SIZE))
        }
    }
}

/// One `vkQueueBindSparse` under the queue's lock, ordered after every earlier internal batch
/// and before every later submission on any queue ([`InternalWork`]): binds take effect for work
/// submitted afterwards, as the API promises.
///
/// # Safety
/// As `contract::Queue::bind_sparse`.
pub(crate) unsafe fn bind_sparse(queue: &Queue, binds: &[SparseBind<'_>]) -> Result<()> {
    let s = queue.shared();
    s.lost.check()?;
    if s.families.properties[queue.family as usize].queueFlags & VK_QUEUE_SPARSE_BINDING_BIT == 0 {
        return Err(unsupported());
    }
    // Every bind's memory range, then the infos that point at them (no reallocation after).
    let mut buffer_binds: Vec<(VkBuffer, VkSparseMemoryBind)> = Vec::new();
    let mut opaque_binds: Vec<(VkImage, VkSparseMemoryBind)> = Vec::new();
    let mut image_binds: Vec<(VkImage, VkSparseImageMemoryBind)> = Vec::new();
    for bind in binds {
        match *bind {
            SparseBind::Buffer {
                buffer,
                tile_offset,
                tile_count,
                memory,
            } => {
                let raw = buffer.raw.raw();
                let req = buffer_raw_requirements(s, raw);
                let (memory, memory_offset) = bind_memory(memory, req.memoryTypeBits)?;
                let offset = tile_offset as u64 * SPARSE_TILE_SIZE;
                // The last tile may reach past the buffer's (block-rounded) size.
                let size = (tile_count as u64 * SPARSE_TILE_SIZE).min(req.size - offset);
                buffer_binds.push((
                    raw,
                    VkSparseMemoryBind {
                        resourceOffset: offset,
                        size,
                        memory,
                        memoryOffset: memory_offset,
                        flags: 0,
                    },
                ));
            }
            SparseBind::Texture {
                texture,
                region,
                memory,
            } => {
                let t = &texture.raw;
                let sparse = t.0.sparse.expect("a sparse texture");
                let (memory, memory_offset) = bind_memory(memory, sparse.memory_type_bits)?;
                match region {
                    SparseTextureRegion::Tiles {
                        mip,
                        layer,
                        origin,
                        size,
                    } => {
                        let g = sparse.granularity;
                        let d = &texture.desc;
                        let (w, h) = d.mip_size(mip);
                        let extent = [w, h, d.mip_depth(mip)];
                        let tile = [g.width, g.height, g.depth];
                        let offset: [u32; 3] = core::array::from_fn(|i| origin[i] * tile[i]);
                        // Edge tiles stop at the mip's edge.
                        let size: [u32; 3] = core::array::from_fn(|i| {
                            (size[i] * tile[i]).min(extent[i] - offset[i])
                        });
                        image_binds.push((
                            t.image(),
                            VkSparseImageMemoryBind {
                                subresource: VkImageSubresource {
                                    aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                                    mipLevel: mip,
                                    arrayLayer: layer,
                                },
                                offset: VkOffset3D {
                                    x: offset[0] as i32,
                                    y: offset[1] as i32,
                                    z: offset[2] as i32,
                                },
                                extent: VkExtent3D {
                                    width: size[0],
                                    height: size[1],
                                    depth: size[2],
                                },
                                memory,
                                memoryOffset: memory_offset,
                                flags: 0,
                            },
                        ));
                    }
                    SparseTextureRegion::PackedMips { layer } => {
                        // One tail for every layer: layer 0's bind covers them all.
                        if sparse.single_tail && layer > 0 {
                            continue;
                        }
                        let stride = if sparse.single_tail {
                            0
                        } else {
                            sparse.tail_stride
                        };
                        opaque_binds.push((
                            t.image(),
                            VkSparseMemoryBind {
                                resourceOffset: sparse.tail_offset + layer as u64 * stride,
                                size: sparse.tail_size,
                                memory,
                                memoryOffset: memory_offset,
                                flags: 0,
                            },
                        ));
                    }
                }
            }
        }
    }
    let buffer_infos: Vec<VkSparseBufferMemoryBindInfo> = buffer_binds
        .iter()
        .map(|(buffer, bind)| VkSparseBufferMemoryBindInfo {
            buffer: *buffer,
            bindCount: 1,
            pBinds: bind,
        })
        .collect();
    let opaque_infos: Vec<VkSparseImageOpaqueMemoryBindInfo> = opaque_binds
        .iter()
        .map(|(image, bind)| VkSparseImageOpaqueMemoryBindInfo {
            image: *image,
            bindCount: 1,
            pBinds: bind,
        })
        .collect();
    let image_infos: Vec<VkSparseImageMemoryBindInfo> = image_binds
        .iter()
        .map(|(image, bind)| VkSparseImageMemoryBindInfo {
            image: *image,
            bindCount: 1,
            pBinds: bind,
        })
        .collect();
    queue.with_lock(|raw, _| {
        s.internal.ordered(|semaphore, wait, signal| {
            let values = VkTimelineSemaphoreSubmitInfo {
                waitSemaphoreValueCount: 1,
                pWaitSemaphoreValues: &wait,
                signalSemaphoreValueCount: 1,
                pSignalSemaphoreValues: &signal,
                ..Default::default()
            };
            let info = VkBindSparseInfo {
                pNext: (&raw const values).cast(),
                waitSemaphoreCount: 1,
                pWaitSemaphores: &semaphore,
                bufferBindCount: buffer_infos.len() as u32,
                pBufferBinds: buffer_infos.as_ptr(),
                imageOpaqueBindCount: opaque_infos.len() as u32,
                pImageOpaqueBinds: opaque_infos.as_ptr(),
                imageBindCount: image_infos.len() as u32,
                pImageBinds: image_infos.as_ptr(),
                signalSemaphoreCount: 1,
                pSignalSemaphores: &semaphore,
                ..Default::default()
            };
            // SAFETY: the queue is locked and supports sparse binding; the ranges were checked by
            // `api.rs` and the caller keeps the heaps alive.
            s.lost
                .result(unsafe { (s.fns.vkQueueBindSparse)(raw, 1, &info, 0) })
        })
    })
}

// --- Internal queue work (resource area) ---

/// Queue work the backend does on its own that later submissions must follow: new textures leave
/// `VK_IMAGE_LAYOUT_UNDEFINED` for GENERAL (`Layout::Common`, where the API says a new texture
/// is), and sparse binds. Each batch waits for the last value of one timeline semaphore and
/// signals the next (so the values rise in execution order across queues); every submission, on
/// any queue, waits for the last value.
///
/// New textures are batched: the next submission on any queue first submits one small command
/// buffer of `UNDEFINED → GENERAL` barriers for every texture created since. That costs one extra
/// `vkQueueSubmit2` per submission that follows texture creation, and nothing otherwise. Host
/// transitions (`vkTransitionImageLayout`, host image copy) would avoid the submission but need
/// `VK_IMAGE_USAGE_HOST_TRANSFER_BIT` on every texture, which costs some drivers their
/// compression (`optimalDeviceAccess`), and not every 1.4 device has them. A texture dropped
/// while its batch may still run waits for that batch (its own signal), so the image is never
/// destroyed under it.
pub(crate) struct InternalWork {
    timeline: VkSemaphore,
    state: Mutex<WorkState>,
}

#[derive(Default)]
struct WorkState {
    /// The value the last submitted batch signals.
    value: u64,
    /// New textures still UNDEFINED.
    pending: Vec<(VkImage, VkImageAspectFlags)>,
    /// Textures whose transition was submitted in the batch signaling this value.
    flushed: Vec<(VkImage, u64)>,
    pools: Vec<FamilyPool>,
}

/// The transition command buffers of one queue family, each with the value of the batch that
/// last used it.
struct FamilyPool {
    family: u32,
    raw: VkCommandPool,
    buffers: Vec<(VkCommandBuffer, u64)>,
}

impl InternalWork {
    pub(crate) fn new(fns: &DeviceFns, device: VkDevice) -> Result<Self> {
        let timeline = VkSemaphoreTypeCreateInfo {
            semaphoreType: VK_SEMAPHORE_TYPE_TIMELINE,
            initialValue: 0,
            ..Default::default()
        };
        let info = VkSemaphoreCreateInfo {
            pNext: (&raw const timeline).cast(),
            ..Default::default()
        };
        let mut raw = 0;
        // SAFETY: a valid create info chain and out pointer.
        super::check(unsafe { (fns.vkCreateSemaphore)(device, &info, ptr::null(), &mut raw) })?;
        Ok(Self {
            timeline: raw,
            state: Mutex::new(WorkState::default()),
        })
    }

    /// # Safety
    /// Called once, when the device is idle and about to be destroyed.
    pub(crate) unsafe fn destroy(&self, fns: &DeviceFns, device: VkDevice) {
        let state = self.state.lock();
        // SAFETY: forwarded from the caller; destroying a pool frees its command buffers.
        unsafe {
            for pool in &state.pools {
                (fns.vkDestroyCommandPool)(device, pool.raw, ptr::null());
            }
            (fns.vkDestroySemaphore)(device, self.timeline, ptr::null());
        }
    }

    fn completed(&self, shared: &Shared) -> u64 {
        let mut value = 0;
        // SAFETY: a live timeline semaphore and out pointer.
        let r = unsafe {
            (shared.fns.vkGetSemaphoreCounterValue)(shared.raw, self.timeline, &mut value)
        };
        let _ = shared.lost.result(r);
        value
    }

    /// Queues a new texture's move to GENERAL.
    fn add(&self, image: VkImage, aspect: VkImageAspectFlags) {
        self.state.lock().pending.push((image, aspect));
    }

    /// Takes a texture about to be destroyed off the queue, or waits for the batch that moved it.
    fn forget(&self, shared: &Shared, image: VkImage) {
        let batch = {
            let mut state = self.state.lock();
            if let Some(i) = state.pending.iter().position(|p| p.0 == image) {
                state.pending.swap_remove(i);
                return;
            }
            match state.flushed.iter().position(|f| f.0 == image) {
                Some(i) => state.flushed.swap_remove(i).1,
                None => return,
            }
        };
        if shared.lost.is_lost() || self.completed(shared) >= batch {
            return;
        }
        let info = VkSemaphoreWaitInfo {
            semaphoreCount: 1,
            pSemaphores: &self.timeline,
            pValues: &batch,
            ..Default::default()
        };
        // SAFETY: a live timeline semaphore whose signal of `batch` was submitted.
        let r = unsafe { (shared.fns.vkWaitSemaphores)(shared.raw, &info, u64::MAX) };
        let _ = shared.lost.result(r);
    }

    /// Runs `f(semaphore, wait, signal)` (a queue operation waiting for the last value and
    /// signaling the next) with the state locked; the next value becomes the last if it succeeds.
    pub(crate) fn ordered(
        &self,
        f: impl FnOnce(VkSemaphore, u64, u64) -> Result<()>,
    ) -> Result<()> {
        let mut state = self.state.lock();
        f(self.timeline, state.value, state.value + 1)?;
        state.value += 1;
        Ok(())
    }

    /// Called by `Queue::submit` with the queue locked: submits the pending texture transitions
    /// on `queue` (of `family`) first, if any, and returns the wait every submission needs.
    pub(crate) fn before_submit(
        &self,
        shared: &Shared,
        queue: VkQueue,
        family: u32,
    ) -> Result<Option<(VkSemaphore, u64)>> {
        let mut state = self.state.lock();
        if !state.pending.is_empty() {
            let completed = self.completed(shared);
            state.flushed.retain(|f| f.1 > completed);
            let (pool, slot) = self.transition_buffer(shared, &mut state, family, completed)?;
            let buffer = state.pools[pool].buffers[slot].0;
            let (wait, signal) = (state.value, state.value + 1);
            let wait_info = VkSemaphoreSubmitInfo {
                semaphore: self.timeline,
                value: wait,
                stageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                ..Default::default()
            };
            let signal_info = VkSemaphoreSubmitInfo {
                value: signal,
                ..wait_info
            };
            let list = VkCommandBufferSubmitInfo {
                commandBuffer: buffer,
                ..Default::default()
            };
            let info = VkSubmitInfo2 {
                waitSemaphoreInfoCount: 1,
                pWaitSemaphoreInfos: &wait_info,
                commandBufferInfoCount: 1,
                pCommandBufferInfos: &list,
                signalSemaphoreInfoCount: 1,
                pSignalSemaphoreInfos: &signal_info,
                ..Default::default()
            };
            // SAFETY: the caller holds the queue's lock; the buffer is recorded and of `family`.
            let r = unsafe { (shared.fns.vkQueueSubmit2)(queue, 1, &info, 0) };
            shared.lost.result(r)?;
            state.value = signal;
            let flushed: Vec<(VkImage, u64)> = state
                .pending
                .drain(..)
                .map(|(image, _)| (image, signal))
                .collect();
            state.flushed.extend(flushed);
            state.pools[pool].buffers[slot].1 = signal;
        }
        Ok((state.value > 0).then_some((self.timeline, state.value)))
    }

    /// Records the pending transitions into a free command buffer of `family`'s pool; returns
    /// the pool's and the buffer's index.
    fn transition_buffer(
        &self,
        shared: &Shared,
        state: &mut WorkState,
        family: u32,
        completed: u64,
    ) -> Result<(usize, usize)> {
        let f = &shared.fns;
        let pool = match state.pools.iter().position(|p| p.family == family) {
            Some(i) => i,
            None => {
                let info = VkCommandPoolCreateInfo {
                    flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT
                        | VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
                    queueFamilyIndex: family,
                    ..Default::default()
                };
                let mut raw = 0;
                // SAFETY: a valid create info and out pointer.
                let r =
                    unsafe { (f.vkCreateCommandPool)(shared.raw, &info, ptr::null(), &mut raw) };
                shared.lost.result(r)?;
                state.pools.push(FamilyPool {
                    family,
                    raw,
                    buffers: Vec::new(),
                });
                state.pools.len() - 1
            }
        };
        let index = pool;
        let pool = &mut state.pools[index];
        let slot = match pool.buffers.iter().position(|b| b.1 <= completed) {
            Some(i) => i,
            None => {
                let info = VkCommandBufferAllocateInfo {
                    commandPool: pool.raw,
                    level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                    commandBufferCount: 1,
                    ..Default::default()
                };
                let mut buffer = ptr::null_mut();
                // SAFETY: a valid allocate info and out pointer; the pool is locked with `state`.
                let r = unsafe { (f.vkAllocateCommandBuffers)(shared.raw, &info, &mut buffer) };
                shared.lost.result(r)?;
                pool.buffers.push((buffer, 0));
                pool.buffers.len() - 1
            }
        };
        let buffer = pool.buffers[slot].0;
        let barriers: Vec<VkImageMemoryBarrier2> = state
            .pending
            .iter()
            .map(|&(image, aspect)| VkImageMemoryBarrier2 {
                srcStageMask: VK_PIPELINE_STAGE_2_NONE,
                srcAccessMask: VK_ACCESS_2_NONE,
                dstStageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                dstAccessMask: VK_ACCESS_2_MEMORY_READ_BIT | VK_ACCESS_2_MEMORY_WRITE_BIT,
                oldLayout: VK_IMAGE_LAYOUT_UNDEFINED,
                newLayout: VK_IMAGE_LAYOUT_GENERAL,
                srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
                image,
                subresourceRange: VkImageSubresourceRange {
                    aspectMask: aspect,
                    baseMipLevel: 0,
                    levelCount: u32::MAX,
                    baseArrayLayer: 0,
                    layerCount: u32::MAX,
                },
                ..Default::default()
            })
            .collect();
        let dependency = VkDependencyInfo {
            imageMemoryBarrierCount: barriers.len() as u32,
            pImageMemoryBarriers: barriers.as_ptr(),
            ..Default::default()
        };
        let begin = VkCommandBufferBeginInfo {
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            ..Default::default()
        };
        // SAFETY: a command buffer no batch uses any more (its value has completed), of a pool
        // that resets buffers on begin; live images.
        unsafe {
            shared
                .lost
                .result((f.vkBeginCommandBuffer)(buffer, &begin))?;
            (f.vkCmdPipelineBarrier2)(buffer, &dependency);
            shared.lost.result((f.vkEndCommandBuffer)(buffer))?;
        }
        Ok((index, slot))
    }
}
