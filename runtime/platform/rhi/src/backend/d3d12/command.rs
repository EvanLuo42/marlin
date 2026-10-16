//! Command pools and lists: barriers, render passes, copies, draws and dispatches.

use core::ffi::c_void;
use core::ptr;

use alloc::sync::Arc;

use super::com::{self, *};
use super::ffi::*;
use super::pipeline::{Bindless, ComputePipeline, GraphicsPipeline, heaps};
use super::resource::Texture;
use super::{Backend, Device, check, error};
use crate::{
    Access, Buffer, BufferBarrier, BufferTextureCopy, IndexFormat, Layout, LoadOp,
    MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, Rect, RenderPassDesc, Result, SubresourceRange,
    Texture as PublicTexture, TextureBarrier, Viewport,
};

pub(crate) struct CommandPool {
    allocator: Com<ID3D12CommandAllocator>,
}

impl CommandPool {
    pub(super) fn new(device: &Com<ID3D12Device>) -> Result<Self> {
        // SAFETY: valid interface, IID and out pointer.
        let allocator = com::create(|iid, out| unsafe {
            (device.vtbl().CreateCommandAllocator)(
                device.as_raw(),
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                iid,
                out,
            )
        })
        .map_err(error)?;
        Ok(Self { allocator })
    }
}

pub(crate) struct CommandList {
    list: Com<ID3D12GraphicsCommandList>,
    /// Present when the device supports enhanced barriers.
    list7: Option<Com<ID3D12GraphicsCommandList7>>,
    bindless: Arc<Bindless>,
    /// The last pipeline bound was a compute pipeline: push constants go to the compute slot.
    compute: bool,
}

impl CommandList {
    pub(super) fn new(device: &Device, pool: &CommandPool) -> Result<Self> {
        let d = &device.device;
        // SAFETY: valid interfaces, IID and out pointer; no initial pipeline state.
        let list = com::create::<ID3D12GraphicsCommandList>(|iid, out| unsafe {
            (d.vtbl().CreateCommandList)(
                d.as_raw(),
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                pool.allocator.as_raw(),
                ptr::null_mut(),
                iid,
                out,
            )
        })
        .map_err(error)?;
        // Lists are created recording; `begin` expects them closed.
        // SAFETY: valid interface.
        check(unsafe { (list.vtbl().Close)(list.as_raw()) })?;
        let list7 = match device.features.enhanced_barriers {
            true => list.cast().ok(),
            false => None,
        };
        Ok(Self {
            list,
            list7,
            bindless: device.bindless.clone(),
            compute: false,
        })
    }

    pub(super) fn as_raw(&self) -> *mut c_void {
        self.list.as_raw()
    }
}

/// Maps a layout to the legacy resource state that stands for it.
fn legacy_state(layout: Layout) -> u32 {
    match layout {
        Layout::Undefined | Layout::Present | Layout::Common => D3D12_RESOURCE_STATE_COMMON,
        Layout::RenderTarget => D3D12_RESOURCE_STATE_RENDER_TARGET,
        Layout::UnorderedAccess => D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        Layout::DepthStencilWrite => D3D12_RESOURCE_STATE_DEPTH_WRITE,
        Layout::DepthStencilRead => D3D12_RESOURCE_STATE_DEPTH_READ,
        Layout::ShaderResource => {
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
        }
        Layout::CopySrc => D3D12_RESOURCE_STATE_COPY_SOURCE,
        Layout::CopyDst => D3D12_RESOURCE_STATE_COPY_DEST,
        Layout::ResolveSrc => D3D12_RESOURCE_STATE_RESOLVE_SOURCE,
        Layout::ResolveDst => D3D12_RESOURCE_STATE_RESOLVE_DEST,
    }
}

/// Maps buffer accesses to the legacy resource state that allows them all.
fn legacy_buffer_state(access: Access) -> u32 {
    const MAP: [(Access, u32); 7] = [
        (
            Access::VERTEX_BUFFER.union(Access::CONSTANT_BUFFER),
            D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER,
        ),
        (Access::INDEX_BUFFER, D3D12_RESOURCE_STATE_INDEX_BUFFER),
        (
            Access::UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        ),
        (
            Access::SHADER_RESOURCE,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        ),
        (
            Access::INDIRECT_ARGUMENT,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
        ),
        (Access::COPY_DST, D3D12_RESOURCE_STATE_COPY_DEST),
        (Access::COPY_SRC, D3D12_RESOURCE_STATE_COPY_SOURCE),
    ];
    MAP.iter()
        .filter(|(a, _)| access.bits() & a.bits() != 0)
        .fold(D3D12_RESOURCE_STATE_COMMON, |s, (_, state)| s | state)
}

fn barrier_layout(layout: Layout) -> u32 {
    match layout {
        Layout::Undefined => D3D12_BARRIER_LAYOUT_UNDEFINED,
        // PRESENT and COMMON are the same value.
        Layout::Present | Layout::Common => D3D12_BARRIER_LAYOUT_COMMON,
        Layout::RenderTarget => D3D12_BARRIER_LAYOUT_RENDER_TARGET,
        Layout::UnorderedAccess => D3D12_BARRIER_LAYOUT_UNORDERED_ACCESS,
        Layout::DepthStencilWrite => D3D12_BARRIER_LAYOUT_DEPTH_STENCIL_WRITE,
        Layout::DepthStencilRead => D3D12_BARRIER_LAYOUT_DEPTH_STENCIL_READ,
        Layout::ShaderResource => D3D12_BARRIER_LAYOUT_SHADER_RESOURCE,
        Layout::CopySrc => D3D12_BARRIER_LAYOUT_COPY_SOURCE,
        Layout::CopyDst => D3D12_BARRIER_LAYOUT_COPY_DEST,
        Layout::ResolveSrc => D3D12_BARRIER_LAYOUT_RESOLVE_SOURCE,
        Layout::ResolveDst => D3D12_BARRIER_LAYOUT_RESOLVE_DEST,
    }
}

/// `Access` bits equal D3D12's, except that none is `NO_ACCESS` (D3D12's 0 means "common").
fn barrier_access(access: Access) -> u32 {
    match access.bits() {
        0 => D3D12_BARRIER_ACCESS_NO_ACCESS,
        bits => bits,
    }
}

/// Resolves `u32::MAX` counts against the texture's extent.
fn resolve(range: SubresourceRange, texture: &Texture) -> (u32, u32, u32, u32) {
    let mips = match range.mip_count {
        u32::MAX => texture.mip_levels.saturating_sub(range.base_mip),
        n => n,
    };
    let layers = match range.layer_count {
        u32::MAX => texture.array_layers.saturating_sub(range.base_layer),
        n => n,
    };
    (range.base_mip, mips, range.base_layer, layers)
}

/// Batches barrier structs on the stack, flushing every `N`.
struct Batch<T, const N: usize> {
    items: [T; N],
    len: usize,
}

impl<T: Copy, const N: usize> Batch<T, N> {
    fn new(empty: T) -> Self {
        Self {
            items: [empty; N],
            len: 0,
        }
    }

    fn push(&mut self, item: T, flush: &mut impl FnMut(&[T])) {
        if self.len == N {
            flush(&self.items);
            self.len = 0;
        }
        self.items[self.len] = item;
        self.len += 1;
    }

    fn finish(&mut self, flush: &mut impl FnMut(&[T])) {
        if self.len > 0 {
            flush(&self.items[..self.len]);
            self.len = 0;
        }
    }
}

const EMPTY_TRANSITION: D3D12_RESOURCE_BARRIER = D3D12_RESOURCE_BARRIER {
    Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    Flags: 0,
    Transition: D3D12_RESOURCE_TRANSITION_BARRIER {
        pResource: ptr::null_mut(),
        Subresource: 0,
        StateBefore: 0,
        StateAfter: 0,
    },
};

/// A UAV barrier: the UAV member shares the transition member's first field.
fn uav_barrier(resource: *mut c_void) -> D3D12_RESOURCE_BARRIER {
    let mut uav = EMPTY_TRANSITION;
    uav.Type = 2;
    uav.Transition.pResource = resource;
    uav
}

fn legacy_flush(list: &Com<ID3D12GraphicsCommandList>) -> impl FnMut(&[D3D12_RESOURCE_BARRIER]) {
    move |items| {
        // SAFETY: valid interface; `items` outlives the call.
        unsafe { (list.vtbl().ResourceBarrier)(list.as_raw(), items.len() as u32, items.as_ptr()) };
    }
}

fn enhanced_flush<T>(list: &Com<ID3D12GraphicsCommandList7>, ty: i32) -> impl FnMut(&[T]) {
    move |items| {
        let group = D3D12_BARRIER_GROUP {
            Type: ty,
            NumBarriers: items.len() as u32,
            pBarriers: items.as_ptr().cast(),
        };
        // SAFETY: valid interface; `group` points at `items` of the type `ty` names, which
        // outlive the call.
        unsafe { (list.vtbl().Barrier)(list.as_raw(), 1, &group) };
    }
}

/// The subresource index of mip `mip`, layer `layer` (plane 0).
fn subresource(texture: &Texture, mip: u32, layer: u32) -> u32 {
    mip + layer * texture.mip_levels
}

/// Buffer footprints of depth textures use the typeless plane format.
fn copy_format(texture: &Texture) -> u32 {
    match texture.format {
        DXGI_FORMAT_D32_FLOAT => DXGI_FORMAT_R32_TYPELESS,
        format => format,
    }
}

fn footprint(
    buffer: &Buffer,
    texture: &PublicTexture,
    r: &BufferTextureCopy,
) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: buffer.raw.resource.as_raw(),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            Offset: r.buffer_offset,
            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                Format: copy_format(&texture.raw),
                Width: r.width,
                Height: r.height,
                Depth: 1,
                RowPitch: r.bytes_per_row,
            },
        },
    }
}

fn texture_location(texture: &PublicTexture, r: &BufferTextureCopy) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: texture.raw.resource.as_raw(),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            // `SubresourceIndex` overlays the low half of `Offset`.
            Offset: subresource(&texture.raw, r.mip, r.layer) as u64,
            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                Format: 0,
                Width: 0,
                Height: 0,
                Depth: 0,
                RowPitch: 0,
            },
        },
    }
}

impl crate::contract::Commands for Backend {
    unsafe fn reset_pool(pool: &mut CommandPool) -> Result<()> {
        let a = &pool.allocator;
        // SAFETY: the caller guarantees the GPU finished the pool's lists.
        check(unsafe { (a.vtbl().Reset)(a.as_raw()) })
    }

    fn begin(list: &mut CommandList, pool: &mut CommandPool) -> Result<()> {
        let l = &list.list;
        // SAFETY: valid interfaces; D3D12 itself rejects resetting a list still recording.
        check(unsafe { (l.vtbl().Reset)(l.as_raw(), pool.allocator.as_raw(), ptr::null_mut()) })?;
        list.compute = false;
        let b = &list.bindless;
        let heaps = heaps(b);
        let zeros = [0u32; MAX_PUSH_CONSTANTS as usize];
        // SAFETY: valid interfaces. Directly indexed heaps must be bound before the root
        // signature. Zeroing the constants keeps the debug layer from flagging an unset root
        // parameter in shaders that ignore them.
        unsafe {
            (l.vtbl().SetDescriptorHeaps)(l.as_raw(), 2, heaps.as_ptr());
            (l.vtbl().SetGraphicsRootSignature)(l.as_raw(), b.root.as_raw());
            (l.vtbl().SetComputeRootSignature)(l.as_raw(), b.root.as_raw());
            (l.vtbl().SetGraphicsRoot32BitConstants)(
                l.as_raw(),
                0,
                MAX_PUSH_CONSTANTS,
                zeros.as_ptr().cast(),
                0,
            );
            (l.vtbl().SetComputeRoot32BitConstants)(
                l.as_raw(),
                0,
                MAX_PUSH_CONSTANTS,
                zeros.as_ptr().cast(),
                0,
            );
        }
        Ok(())
    }

    fn end(list: &mut CommandList) -> Result<()> {
        // SAFETY: valid interface.
        check(unsafe { (list.list.vtbl().Close)(list.list.as_raw()) })
    }

    fn texture_barriers(list: &mut CommandList, barriers: &[TextureBarrier<'_>]) {
        if let Some(l7) = &list.list7 {
            let mut flush = enhanced_flush(l7, D3D12_BARRIER_TYPE_TEXTURE);
            const EMPTY: D3D12_TEXTURE_BARRIER = D3D12_TEXTURE_BARRIER {
                SyncBefore: 0,
                SyncAfter: 0,
                AccessBefore: 0,
                AccessAfter: 0,
                LayoutBefore: 0,
                LayoutAfter: 0,
                pResource: ptr::null_mut(),
                Subresources: D3D12_BARRIER_SUBRESOURCE_RANGE {
                    IndexOrFirstMipLevel: 0,
                    NumMipLevels: 0,
                    FirstArraySlice: 0,
                    NumArraySlices: 0,
                    FirstPlane: 0,
                    NumPlanes: 0,
                },
                Flags: 0,
            };
            let mut batch = Batch::<_, 16>::new(EMPTY);
            for b in barriers {
                let texture = &b.texture.raw;
                let subresources = if b.range == SubresourceRange::ALL {
                    // NumMipLevels 0 makes the first field a subresource index; all-ones is "all".
                    D3D12_BARRIER_SUBRESOURCE_RANGE {
                        IndexOrFirstMipLevel: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        ..EMPTY.Subresources
                    }
                } else {
                    let (mip, mips, layer, layers) = resolve(b.range, texture);
                    D3D12_BARRIER_SUBRESOURCE_RANGE {
                        IndexOrFirstMipLevel: mip,
                        NumMipLevels: mips,
                        FirstArraySlice: layer,
                        NumArraySlices: layers,
                        FirstPlane: 0,
                        NumPlanes: 1,
                    }
                };
                batch.push(
                    D3D12_TEXTURE_BARRIER {
                        SyncBefore: b.before.stage.bits(),
                        SyncAfter: b.after.stage.bits(),
                        AccessBefore: barrier_access(b.before.access),
                        AccessAfter: barrier_access(b.after.access),
                        LayoutBefore: barrier_layout(b.before.layout),
                        LayoutAfter: barrier_layout(b.after.layout),
                        pResource: texture.resource.as_raw(),
                        Subresources: subresources,
                        Flags: 0,
                    },
                    &mut flush,
                );
            }
            batch.finish(&mut flush);
            return;
        }

        // Legacy: one transition per subresource whose state changes, or a UAV barrier between
        // unordered-access uses.
        let mut flush = legacy_flush(&list.list);
        let mut batch = Batch::<_, 16>::new(EMPTY_TRANSITION);
        for b in barriers {
            let texture = &b.texture.raw;
            let resource = texture.resource.as_raw();
            let before = legacy_state(b.before.layout);
            let after = legacy_state(b.after.layout);
            if before == after {
                if before == D3D12_RESOURCE_STATE_UNORDERED_ACCESS {
                    batch.push(uav_barrier(resource), &mut flush);
                }
                continue;
            }
            let mut transition = |subresource| {
                let mut t = EMPTY_TRANSITION;
                t.Transition = D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: resource,
                    Subresource: subresource,
                    StateBefore: before,
                    StateAfter: after,
                };
                batch.push(t, &mut flush);
            };
            if b.range == SubresourceRange::ALL {
                transition(D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES);
            } else {
                let (mip, mips, layer, layers) = resolve(b.range, texture);
                for l in layer..layer + layers {
                    for m in mip..mip + mips {
                        transition(m + l * texture.mip_levels);
                    }
                }
            }
        }
        batch.finish(&mut flush);
    }

    fn buffer_barriers(list: &mut CommandList, barriers: &[BufferBarrier<'_>]) {
        if let Some(l7) = &list.list7 {
            let mut flush = enhanced_flush(l7, D3D12_BARRIER_TYPE_BUFFER);
            let mut batch = Batch::<_, 16>::new(D3D12_BUFFER_BARRIER {
                SyncBefore: 0,
                SyncAfter: 0,
                AccessBefore: 0,
                AccessAfter: 0,
                pResource: ptr::null_mut(),
                Offset: 0,
                Size: 0,
            });
            for b in barriers {
                batch.push(
                    D3D12_BUFFER_BARRIER {
                        SyncBefore: b.before.stage.bits(),
                        SyncAfter: b.after.stage.bits(),
                        AccessBefore: barrier_access(b.before.access),
                        AccessAfter: barrier_access(b.after.access),
                        pResource: b.buffer.raw.resource.as_raw(),
                        // Whole buffer: offset 0, size UINT64_MAX.
                        Offset: 0,
                        Size: u64::MAX,
                    },
                    &mut flush,
                );
            }
            batch.finish(&mut flush);
            return;
        }

        // Legacy: buffers start each submission in the common state and are promoted on first
        // use, so a barrier without a prior access has nothing to do. Upload and readback
        // buffers never leave their initial state.
        let mut flush = legacy_flush(&list.list);
        let mut batch = Batch::<_, 16>::new(EMPTY_TRANSITION);
        for b in barriers {
            if b.buffer.raw.host || b.before.access == Access::NONE {
                continue;
            }
            let resource = b.buffer.raw.resource.as_raw();
            let before = legacy_buffer_state(b.before.access);
            let after = legacy_buffer_state(b.after.access);
            if before == after {
                if before == D3D12_RESOURCE_STATE_UNORDERED_ACCESS {
                    batch.push(uav_barrier(resource), &mut flush);
                }
                continue;
            }
            let mut t = EMPTY_TRANSITION;
            t.Transition = D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: resource,
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            };
            batch.push(t, &mut flush);
        }
        batch.finish(&mut flush);
    }

    fn begin_render_pass(list: &mut CommandList, desc: &RenderPassDesc<'_>) {
        let l = &list.list;
        let mut handles = [D3D12_CPU_DESCRIPTOR_HANDLE::default(); MAX_COLOR_ATTACHMENTS];
        for (h, a) in handles.iter_mut().zip(desc.color) {
            *h = a.view.raw.handle;
        }
        let depth = desc.depth.map(|d| d.view.raw.slot.handle());
        // SAFETY: valid interface; the handles are live descriptors (borrowed views).
        unsafe {
            (l.vtbl().OMSetRenderTargets)(
                l.as_raw(),
                desc.color.len() as u32,
                handles.as_ptr(),
                0,
                depth.as_ref().map_or(ptr::null(), |d| d as *const _),
            )
        };
        for a in desc.color {
            if let LoadOp::Clear(color) = a.load {
                // SAFETY: valid interface and descriptor; `color` has 4 floats.
                unsafe {
                    (l.vtbl().ClearRenderTargetView)(
                        l.as_raw(),
                        a.view.raw.handle,
                        color.as_ptr(),
                        0,
                        ptr::null(),
                    )
                };
            }
        }
        if let (Some(handle), Some(d)) = (depth, desc.depth)
            && let LoadOp::Clear(value) = d.load
        {
            // SAFETY: valid interface and descriptor.
            unsafe {
                (l.vtbl().ClearDepthStencilView)(
                    l.as_raw(),
                    handle,
                    D3D12_CLEAR_FLAG_DEPTH,
                    value,
                    0,
                    0,
                    ptr::null(),
                )
            };
        }
    }

    fn end_render_pass(_: &mut CommandList) {
        // Emulated passes have nothing to resolve or store.
    }

    fn copy_buffer(
        list: &mut CommandList,
        src: &Buffer,
        src_offset: u64,
        dst: &Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        let l = &list.list;
        // SAFETY: valid interfaces; ranges checked by `api.rs`.
        unsafe {
            (l.vtbl().CopyBufferRegion)(
                l.as_raw(),
                dst.raw.resource.as_raw(),
                dst_offset,
                src.raw.resource.as_raw(),
                src_offset,
                size,
            )
        };
    }

    fn copy_buffer_to_texture(
        list: &mut CommandList,
        src: &Buffer,
        dst: &PublicTexture,
        region: &BufferTextureCopy,
    ) {
        let from = footprint(src, dst, region);
        let to = texture_location(dst, region);
        let l = &list.list;
        // SAFETY: valid interface and locations; the region was checked by `api.rs`.
        unsafe {
            (l.vtbl().CopyTextureRegion)(l.as_raw(), &to, region.x, region.y, 0, &from, ptr::null())
        };
    }

    fn copy_texture_to_buffer(
        list: &mut CommandList,
        src: &PublicTexture,
        dst: &Buffer,
        region: &BufferTextureCopy,
    ) {
        let from = texture_location(src, region);
        let to = footprint(dst, src, region);
        let area = D3D12_BOX {
            left: region.x,
            top: region.y,
            front: 0,
            right: region.x + region.width,
            bottom: region.y + region.height,
            back: 1,
        };
        let l = &list.list;
        // SAFETY: valid interface and locations; the region was checked by `api.rs`.
        unsafe { (l.vtbl().CopyTextureRegion)(l.as_raw(), &to, 0, 0, 0, &from, &area) };
    }

    fn set_graphics_pipeline(list: &mut CommandList, pipeline: &GraphicsPipeline) {
        list.compute = false;
        let l = &list.list;
        // SAFETY: valid interfaces.
        unsafe {
            (l.vtbl().SetPipelineState)(l.as_raw(), pipeline.pso.as_raw());
            (l.vtbl().IASetPrimitiveTopology)(l.as_raw(), pipeline.topology);
        }
    }

    fn set_compute_pipeline(list: &mut CommandList, pipeline: &ComputePipeline) {
        list.compute = true;
        let l = &list.list;
        // SAFETY: valid interfaces.
        unsafe { (l.vtbl().SetPipelineState)(l.as_raw(), pipeline.pso.as_raw()) };
    }

    fn push_constants(list: &mut CommandList, offset: u32, data: &[u32]) {
        let l = &list.list;
        let set = match list.compute {
            true => l.vtbl().SetComputeRoot32BitConstants,
            false => l.vtbl().SetGraphicsRoot32BitConstants,
        };
        // SAFETY: valid interface; `offset + data.len()` is within the root constants (checked
        // by `api.rs`).
        unsafe {
            set(
                l.as_raw(),
                0,
                data.len() as u32,
                data.as_ptr().cast(),
                offset,
            )
        };
    }

    fn set_viewport(list: &mut CommandList, viewport: &Viewport) {
        let v = D3D12_VIEWPORT {
            TopLeftX: viewport.x,
            TopLeftY: viewport.y,
            Width: viewport.width,
            Height: viewport.height,
            MinDepth: viewport.min_depth,
            MaxDepth: viewport.max_depth,
        };
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe { (l.vtbl().RSSetViewports)(l.as_raw(), 1, &v) };
    }

    fn set_scissor(list: &mut CommandList, rect: &Rect) {
        let r = D3D12_RECT {
            left: rect.x,
            top: rect.y,
            right: rect.x.saturating_add_unsigned(rect.width),
            bottom: rect.y.saturating_add_unsigned(rect.height),
        };
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe { (l.vtbl().RSSetScissorRects)(l.as_raw(), 1, &r) };
    }

    fn set_index_buffer(list: &mut CommandList, buffer: &Buffer, offset: u64, format: IndexFormat) {
        let view = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: buffer.raw.address + offset,
            SizeInBytes: (buffer.desc.size - offset).min(u32::MAX as u64) as u32,
            Format: match format {
                IndexFormat::U16 => DXGI_FORMAT_R16_UINT,
                IndexFormat::U32 => DXGI_FORMAT_R32_UINT,
            },
        };
        let l = &list.list;
        // SAFETY: valid interface; the view lies inside the buffer.
        unsafe { (l.vtbl().IASetIndexBuffer)(l.as_raw(), &view) };
    }

    fn draw(
        list: &mut CommandList,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe {
            (l.vtbl().DrawInstanced)(
                l.as_raw(),
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            )
        };
    }

    fn draw_indexed(
        list: &mut CommandList,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe {
            (l.vtbl().DrawIndexedInstanced)(
                l.as_raw(),
                index_count,
                instance_count,
                first_index,
                vertex_offset,
                first_instance,
            )
        };
    }

    fn dispatch(list: &mut CommandList, x: u32, y: u32, z: u32) {
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe { (l.vtbl().Dispatch)(l.as_raw(), x, y, z) };
    }
}
