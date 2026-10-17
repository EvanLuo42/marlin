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

pub(crate) fn copy_buffer_to_texture(
    list: &mut CommandList,
    src: &crate::Buffer,
    dst: &crate::Texture,
    region: &BufferTextureCopy,
) {
    let _ = (list, src, dst, region);
    todo!("vulkan: copy_buffer_to_texture")
}

pub(crate) fn copy_texture_to_buffer(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Buffer,
    region: &BufferTextureCopy,
) {
    let _ = (list, src, dst, region);
    todo!("vulkan: copy_texture_to_buffer")
}

pub(crate) fn copy_texture(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Texture,
    region: &TextureCopy,
) {
    let _ = (list, src, dst, region);
    todo!("vulkan: copy_texture")
}

pub(crate) fn resolve_texture(
    list: &mut CommandList,
    src: &crate::Texture,
    dst: &crate::Texture,
    resolve: &TextureResolve,
) {
    let _ = (list, src, dst, resolve);
    todo!("vulkan: resolve_texture")
}

pub(crate) fn clear_storage_texture(
    list: &mut CommandList,
    texture: &crate::Texture,
    range: &SubresourceRange,
    value: ClearColor,
) {
    let _ = (list, texture, range, value);
    todo!("vulkan: clear_storage_texture")
}
