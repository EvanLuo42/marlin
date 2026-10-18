//! Copies, resolves, fills and clears.

use super::command::CommandList;
use super::ffi::*;
use crate::{BufferTextureCopy, ClearColor, SubresourceRange, TextureCopy, TextureResolve};

pub(crate) fn copy_buffer(
    list: &mut CommandList,
    src: &crate::Buffer,
    src_offset: u64,
    dst: &crate::Buffer,
    dst_offset: u64,
    size: u64,
) {
    let region = VkBufferCopy2 {
        srcOffset: src_offset,
        dstOffset: dst_offset,
        size,
        ..Default::default()
    };
    let info = VkCopyBufferInfo2 {
        srcBuffer: src.raw.raw(),
        dstBuffer: dst.raw.raw(),
        regionCount: 1,
        pRegions: &region,
        ..Default::default()
    };
    // SAFETY: a recording buffer; the ranges lie within the buffers (checked by `commands.rs`).
    unsafe { (list.fns().vkCmdCopyBuffer2)(list.raw(), &info) };
}

pub(crate) fn fill_buffer(
    list: &mut CommandList,
    buffer: &crate::Buffer,
    offset: u64,
    size: u64,
    value: u32,
) {
    // SAFETY: a recording buffer; whole words within the buffer (checked by `commands.rs`).
    unsafe { (list.fns().vkCmdFillBuffer)(list.raw(), buffer.raw.raw(), offset, size, value) };
}

// --- Textures (resource area) ---
//
// Copies take their textures in the copy layouts (`Layout::CopySrc` / `CopyDst`, Vulkan's
// TRANSFER_SRC / TRANSFER_DST_OPTIMAL), resolves in the resolve ones, storage clears in
// `Layout::UnorderedAccess` (GENERAL): the states the API's commands document.

use super::command::layout;
use crate::Layout;

fn subresource(aspect: VkImageAspectFlags, mip: u32, layer: u32) -> VkImageSubresourceLayers {
    VkImageSubresourceLayers {
        aspectMask: aspect,
        mipLevel: mip,
        baseArrayLayer: layer,
        layerCount: 1,
    }
}

fn offset(o: [u32; 3]) -> VkOffset3D {
    VkOffset3D {
        x: o[0] as i32,
        y: o[1] as i32,
        z: o[2] as i32,
    }
}

/// The part of a `size` box at `origin` inside mip `mip` of `desc`. Block-compressed regions are
/// whole blocks, padded past the edge of small mips; Vulkan wants them cut at the edge instead.
fn clamped(desc: &crate::TextureDesc, mip: u32, origin: [u32; 3], size: [u32; 3]) -> [u32; 3] {
    let (w, h) = desc.mip_size(mip);
    [
        size[0].min(w.saturating_sub(origin[0])),
        size[1].min(h.saturating_sub(origin[1])),
        size[2],
    ]
}

/// A buffer↔texture region: rows of `bytes_per_row` bytes become `bufferRowLength` texels (whole
/// blocks); depth-stencil formats copy their depth plane.
fn buffer_texture_region(
    texture: &crate::Texture,
    region: &BufferTextureCopy,
) -> VkBufferImageCopy2 {
    let d = &texture.desc;
    let f = d.format;
    let aspect = match f.is_depth() {
        true => VK_IMAGE_ASPECT_DEPTH_BIT,
        false => VK_IMAGE_ASPECT_COLOR_BIT,
    };
    let origin = [region.x, region.y, region.z];
    let size = clamped(
        d,
        region.mip,
        origin,
        [region.width, region.height, region.depth],
    );
    VkBufferImageCopy2 {
        bufferOffset: region.buffer_offset,
        bufferRowLength: region.bytes_per_row / f.copy_block_size() * f.block_extent(),
        // Slices lie `height` texel rows apart.
        bufferImageHeight: region.height,
        imageSubresource: subresource(aspect, region.mip, region.layer),
        imageOffset: offset(origin),
        imageExtent: VkExtent3D {
            width: size[0],
            height: size[1],
            depth: size[2],
        },
        ..Default::default()
    }
}

pub(crate) fn copy_buffer_to_texture(
    list: &mut CommandList,
    src: &crate::Buffer,
    dst: &crate::Texture,
    region: &BufferTextureCopy,
) {
    let r = buffer_texture_region(dst, region);
    let info = VkCopyBufferToImageInfo2 {
        srcBuffer: src.raw.raw(),
        dstImage: dst.raw.image(),
        dstImageLayout: layout(list.shared(), Layout::CopyDst, dst.raw.is_depth()),
        regionCount: 1,
        pRegions: &r,
        ..Default::default()
    };
    // SAFETY: a recording buffer; the region lies within both (checked by `commands.rs`).
    unsafe { (list.fns().vkCmdCopyBufferToImage2)(list.raw(), &info) };
}

pub(crate) fn copy_texture_to_buffer(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Buffer,
    region: &BufferTextureCopy,
) {
    let r = buffer_texture_region(src, region);
    let info = VkCopyImageToBufferInfo2 {
        srcImage: src.raw.image(),
        srcImageLayout: layout(list.shared(), Layout::CopySrc, src.raw.is_depth()),
        dstBuffer: dst.raw.raw(),
        regionCount: 1,
        pRegions: &r,
        ..Default::default()
    };
    // SAFETY: as above.
    unsafe { (list.fns().vkCmdCopyImageToBuffer2)(list.raw(), &info) };
}

/// Depth-stencil copies move both planes (every aspect).
pub(crate) fn copy_texture(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Texture,
    region: &TextureCopy,
) {
    let size = clamped(&src.desc, region.src_mip, region.src_offset, region.size);
    let size = clamped(&dst.desc, region.dst_mip, region.dst_offset, size);
    let r = VkImageCopy2 {
        srcSubresource: subresource(src.raw.aspect(), region.src_mip, region.src_layer),
        srcOffset: offset(region.src_offset),
        dstSubresource: subresource(dst.raw.aspect(), region.dst_mip, region.dst_layer),
        dstOffset: offset(region.dst_offset),
        extent: VkExtent3D {
            width: size[0],
            height: size[1],
            depth: size[2],
        },
        ..Default::default()
    };
    let s = list.shared();
    let info = VkCopyImageInfo2 {
        srcImage: src.raw.image(),
        srcImageLayout: layout(s, Layout::CopySrc, src.raw.is_depth()),
        dstImage: dst.raw.image(),
        dstImageLayout: layout(s, Layout::CopyDst, dst.raw.is_depth()),
        regionCount: 1,
        pRegions: &r,
        ..Default::default()
    };
    // SAFETY: a recording buffer; the boxes lie within both (checked by `commands.rs`).
    unsafe { (list.fns().vkCmdCopyImage2)(list.raw(), &info) };
}

/// `vkCmdResolveImage2` averages in the images' own format, so the sRGB/linear choice of
/// `resolve.format` holds only where it is the textures' format; a twin resolves as the
/// textures' format (Vulkan has no format override for resolves outside render passes).
pub(crate) fn resolve_texture(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Texture,
    resolve: &TextureResolve,
) {
    let (width, height) = src.desc.mip_size(resolve.src_mip);
    let r = VkImageResolve2 {
        srcSubresource: subresource(
            VK_IMAGE_ASPECT_COLOR_BIT,
            resolve.src_mip,
            resolve.src_layer,
        ),
        srcOffset: offset([0; 3]),
        dstSubresource: subresource(
            VK_IMAGE_ASPECT_COLOR_BIT,
            resolve.dst_mip,
            resolve.dst_layer,
        ),
        dstOffset: offset([0; 3]),
        extent: VkExtent3D {
            width,
            height,
            depth: 1,
        },
        ..Default::default()
    };
    let s = list.shared();
    let info = VkResolveImageInfo2 {
        srcImage: src.raw.image(),
        srcImageLayout: layout(s, Layout::ResolveSrc, false),
        dstImage: dst.raw.image(),
        dstImageLayout: layout(s, Layout::ResolveDst, false),
        regionCount: 1,
        pRegions: &r,
        ..Default::default()
    };
    // SAFETY: a recording graphics buffer; same-size color subresources (checked by
    // `commands.rs`).
    unsafe { (list.fns().vkCmdResolveImage2)(list.raw(), &info) };
}

/// `vkCmdClearColorImage` in GENERAL, where storage textures are.
pub(crate) fn clear_storage_texture(
    list: &mut CommandList,
    texture: &crate::Texture,
    range: &SubresourceRange,
    value: ClearColor,
) {
    let color = match value {
        ClearColor::Float(float32) => VkClearColorValue { float32 },
        ClearColor::Uint(uint32) => VkClearColorValue { uint32 },
        ClearColor::Sint(int32) => VkClearColorValue { int32 },
    };
    let range = VkImageSubresourceRange {
        aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
        baseMipLevel: range.base_mip,
        levelCount: range.mip_count,
        baseArrayLayer: range.base_layer,
        layerCount: range.layer_count,
    };
    let general = layout(list.shared(), Layout::UnorderedAccess, false);
    // SAFETY: a recording graphics or compute buffer; a storage texture's range (checked by
    // `commands.rs`).
    unsafe {
        (list.fns().vkCmdClearColorImage)(
            list.raw(),
            texture.raw.image(),
            general,
            &color,
            1,
            &range,
        )
    };
}
