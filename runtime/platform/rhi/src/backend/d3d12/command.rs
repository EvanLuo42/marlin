//! Command pools and lists: barriers, render passes, copies, clears, resolves, draws,
//! dispatches, queries, predication, acceleration structure builds and debug markers.
//!
//! Lists have the type of the queue they run on. Graphics lists bind both root signatures,
//! compute lists the compute one, copy lists neither (they set no shader state).
//!
//! Clears of storage buffers and textures need a view in a shader-visible heap and the same
//! view in a CPU-only one (`ClearUnorderedAccessView*`). Those views are transient: the list
//! keeps their slots until it is begun again, which the API allows only once the GPU has
//! finished the previous recording.

use core::ffi::c_void;
use core::ptr;

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::com::{self, *};
use super::ffi::*;
use super::pipeline::{Bindless, ComputePipeline, GraphicsPipeline, MeshPipeline, heaps};
use super::resource::{self, Descriptor, Texture};
use super::{Backend, Device, QueryPool, check, error};
use crate::{
    AccelerationStructure, AccelerationStructureCopy, Access, Buffer, BufferBarrier, BufferState,
    BufferTextureCopy, BuildAccelerationStructure, ClearColor, IndexFormat, IndirectArgs, Layout,
    LoadOp, MAX_COLOR_ATTACHMENTS, MAX_PUSH_CONSTANTS, MAX_VIEWPORTS, MarkerPoint, PredicateSkip,
    QueueType, Rect, RenderPassDesc, Result, ShadingRate, ShadingRateCombiner, SubresourceRange,
    Texture as PublicTexture, TextureBarrier, TextureCopy, TextureDimension, TextureResolve,
    Viewport,
};

/// The D3D12 command list type of a queue type.
pub(super) fn list_type(ty: QueueType) -> i32 {
    match ty {
        QueueType::Graphics => D3D12_COMMAND_LIST_TYPE_DIRECT,
        QueueType::Compute => D3D12_COMMAND_LIST_TYPE_COMPUTE,
        QueueType::Copy => D3D12_COMMAND_LIST_TYPE_COPY,
    }
}

pub(crate) struct CommandPool {
    allocator: Com<ID3D12CommandAllocator>,
    ty: QueueType,
}

impl CommandPool {
    pub(super) fn new(device: &Com<ID3D12Device>, ty: QueueType) -> Result<Self> {
        // SAFETY: valid interface, IID and out pointer.
        let allocator = com::create(|iid, out| unsafe {
            (device.vtbl().CreateCommandAllocator)(device.as_raw(), list_type(ty), iid, out)
        })
        .map_err(error)?;
        Ok(Self { allocator, ty })
    }

    pub(super) fn as_raw(&self) -> *mut c_void {
        self.allocator.as_raw()
    }
}

pub(crate) struct CommandList {
    list: Com<ID3D12GraphicsCommandList6>,
    /// Present when the device supports enhanced barriers.
    list7: Option<Com<ID3D12GraphicsCommandList7>>,
    device: Com<ID3D12Device>,
    bindless: Arc<Bindless>,
    ty: QueueType,
    /// The last pipeline bound was a compute pipeline: push constants go to the compute slot.
    compute: bool,
    /// Clear views of this recording (shader-visible and CPU-only halves).
    transient: Vec<Descriptor>,
    /// Reused geometry descriptions of acceleration structure builds.
    geometries: Vec<D3D12_RAYTRACING_GEOMETRY_DESC>,
}

impl CommandList {
    pub(super) fn new(device: &Device, pool: &CommandPool) -> Result<Self> {
        let d = &device.device;
        // SAFETY: valid interfaces, IID and out pointer; no initial pipeline state.
        let list = com::create::<ID3D12GraphicsCommandList6>(|iid, out| unsafe {
            (d.vtbl().CreateCommandList)(
                d.as_raw(),
                0,
                list_type(pool.ty),
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
            device: device.device.clone(),
            bindless: device.bindless.clone(),
            ty: pool.ty,
            compute: pool.ty == QueueType::Compute,
            transient: Vec::new(),
            geometries: Vec::new(),
        })
    }

    pub(super) fn as_raw(&self) -> *mut c_void {
        self.list.as_raw()
    }

    fn execute_indirect(&self, signature: &Com<ID3D12CommandSignature>, args: &IndirectArgs<'_>) {
        let (count, count_offset) = match args.count {
            Some((buffer, offset)) => (buffer.raw.resource.as_raw(), offset),
            None => (ptr::null_mut(), 0),
        };
        let l = &self.list;
        // SAFETY: valid interfaces; `api.rs` checked the argument and count ranges.
        unsafe {
            (l.vtbl().ExecuteIndirect)(
                l.as_raw(),
                signature.as_raw(),
                args.max_count,
                args.buffer.raw.resource.as_raw(),
                args.offset,
                count,
                count_offset,
            )
        };
    }

    /// PIX's unformatted-string event encoding: metadata 0, a NUL-terminated UTF-16 string.
    fn event(
        &self,
        name: &str,
        call: unsafe extern "system" fn(*mut c_void, u32, *const c_void, u32),
    ) {
        let mut wide = [0u16; 128];
        let mut n = 0;
        for unit in name.encode_utf16().take(wide.len() - 1) {
            wide[n] = unit;
            n += 1;
        }
        // SAFETY: valid interface; `wide` holds `n + 1` units including the NUL.
        unsafe {
            call(
                self.list.as_raw(),
                0,
                wide.as_ptr().cast(),
                ((n + 1) * size_of::<u16>()) as u32,
            )
        };
    }

    /// A shader-visible and a CPU-only slot for one clear view, kept until the next `begin`.
    fn clear_slots(
        &mut self,
    ) -> Result<(D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_CPU_DESCRIPTOR_HANDLE)> {
        let visible = Descriptor::new(&self.bindless.resources)?;
        let cpu = self.bindless.clear.take()?;
        let handles = (visible.gpu_handle(), cpu.handle());
        self.transient.push(visible);
        self.transient.push(cpu);
        Ok(handles)
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
        Layout::ShadingRateSource => D3D12_RESOURCE_STATE_SHADING_RATE_SOURCE,
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
        // Indirect arguments and predicates share a state.
        (
            Access::INDIRECT_ARGUMENT,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT | D3D12_RESOURCE_STATE_PREDICATION,
        ),
        (Access::COPY_DST, D3D12_RESOURCE_STATE_COPY_DEST),
        (Access::COPY_SRC, D3D12_RESOURCE_STATE_COPY_SOURCE),
    ];
    MAP.iter()
        .filter(|(a, _)| access.intersects(*a))
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
        Layout::ShadingRateSource => D3D12_BARRIER_LAYOUT_SHADING_RATE_SOURCE,
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

fn legacy_flush(list: &Com<ID3D12GraphicsCommandList6>) -> impl FnMut(&[D3D12_RESOURCE_BARRIER]) {
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

/// An aliasing barrier activating `after` (no named predecessor: any overlapping resource).
/// `D3D12_RESOURCE_ALIASING_BARRIER` overlays the transition member: `pResourceBefore` on
/// `pResource`, `pResourceAfter` on the two words after it.
fn aliasing_barrier(after: *mut c_void) -> D3D12_RESOURCE_BARRIER {
    let mut b = EMPTY_TRANSITION;
    b.Type = D3D12_RESOURCE_BARRIER_TYPE_ALIASING;
    let address = after as usize as u64;
    b.Transition.Subresource = address as u32;
    b.Transition.StateBefore = (address >> 32) as u32;
    b
}

/// The subresource index of mip `mip`, layer `layer`, plane `plane`.
fn subresource(texture: &Texture, mip: u32, layer: u32, plane: u32) -> u32 {
    mip + (layer + plane * texture.array_layers) * texture.mip_levels
}

/// Buffer footprints of depth textures use the typeless format of their depth plane.
fn copy_format(texture: &Texture) -> u32 {
    match texture.format {
        DXGI_FORMAT_D32_FLOAT
        | DXGI_FORMAT_D32_FLOAT_S8X24_UINT
        | DXGI_FORMAT_R32G8X24_TYPELESS => DXGI_FORMAT_R32_TYPELESS,
        DXGI_FORMAT_D16_UNORM => DXGI_FORMAT_R16_TYPELESS,
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
                Depth: r.depth,
                RowPitch: r.bytes_per_row,
            },
        },
    }
}

fn subresource_location(texture: &PublicTexture, index: u32) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: texture.raw.resource.as_raw(),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
            // `SubresourceIndex` overlays the low half of `Offset`.
            Offset: index as u64,
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

/// Buffer copies move plane 0 (the depth of depth-stencil formats).
fn texture_location(texture: &PublicTexture, r: &BufferTextureCopy) -> D3D12_TEXTURE_COPY_LOCATION {
    subresource_location(texture, subresource(&texture.raw, r.mip, r.layer, 0))
}

fn shading_rate(rate: ShadingRate) -> i32 {
    rate.encoded() as i32
}

fn combiner(combiner: ShadingRateCombiner) -> i32 {
    match combiner {
        ShadingRateCombiner::Keep => 0,
        ShadingRateCombiner::Replace => 1,
        ShadingRateCombiner::Min => 2,
        ShadingRateCombiner::Max => 3,
        ShadingRateCombiner::Sum => 4,
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
        // The previous recording has finished on the GPU (`begin`'s contract): its clear views
        // are free.
        list.transient.clear();
        list.compute = list.ty == QueueType::Compute;
        if list.ty == QueueType::Copy {
            return Ok(());
        }
        let b = &list.bindless;
        let heaps = heaps(b);
        let zeros = [0u32; MAX_PUSH_CONSTANTS as usize];
        // SAFETY: valid interfaces. Directly indexed heaps must be bound before the root
        // signature. Zeroing the constants keeps the debug layer from flagging an unset root
        // parameter in shaders that ignore them.
        unsafe {
            (l.vtbl().SetDescriptorHeaps)(l.as_raw(), 2, heaps.as_ptr());
            (l.vtbl().SetComputeRootSignature)(l.as_raw(), b.root.as_raw());
            (l.vtbl().SetComputeRoot32BitConstants)(
                l.as_raw(),
                0,
                MAX_PUSH_CONSTANTS,
                zeros.as_ptr().cast(),
                0,
            );
            if list.ty == QueueType::Graphics {
                (l.vtbl().SetGraphicsRootSignature)(l.as_raw(), b.root.as_raw());
                (l.vtbl().SetGraphicsRoot32BitConstants)(
                    l.as_raw(),
                    0,
                    MAX_PUSH_CONSTANTS,
                    zeros.as_ptr().cast(),
                    0,
                );
                (l.vtbl().OMSetBlendFactor)(l.as_raw(), [0.0f32; 4].as_ptr());
            }
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
                        NumPlanes: texture.planes,
                    }
                };
                // Placed render targets and depth taking over aliased memory start with a
                // discard (their metadata is garbage).
                let discard =
                    b.before.layout == Layout::Undefined && texture.placed && texture.target;
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
                        Flags: match discard {
                            true => D3D12_TEXTURE_BARRIER_FLAG_DISCARD,
                            false => 0,
                        },
                    },
                    &mut flush,
                );
            }
            batch.finish(&mut flush);
            return;
        }

        // Legacy: an aliasing barrier when a placed texture takes over its memory, one
        // transition per subresource whose state changes, or a UAV barrier between
        // unordered-access uses. Legacy states name the current state even when the contents
        // are discarded: the caller supplies it (`undefined_from`).
        let mut flush = legacy_flush(&list.list);
        let mut batch = Batch::<_, 16>::new(EMPTY_TRANSITION);
        for b in barriers {
            let texture = &b.texture.raw;
            let resource = texture.resource.as_raw();
            let undefined = b.before.layout == Layout::Undefined;
            if undefined && texture.placed {
                batch.push(aliasing_barrier(resource), &mut flush);
            }
            let before = legacy_state(match undefined {
                true => b.undefined_from,
                false => b.before.layout,
            });
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
                for plane in 0..texture.planes {
                    for l in layer..layer + layers {
                        for m in mip..mip + mips {
                            transition(subresource(texture, m, l, plane));
                        }
                    }
                }
            }
        }
        batch.finish(&mut flush);
        // Reactivated placed targets: discard once they are in their target state.
        for b in barriers {
            let texture = &b.texture.raw;
            let target_state = matches!(
                b.after.layout,
                Layout::RenderTarget | Layout::DepthStencilWrite
            );
            if b.before.layout == Layout::Undefined
                && texture.placed
                && texture.target
                && target_state
            {
                let l = &list.list;
                // SAFETY: valid interfaces; null discards the whole resource, which is in a
                // render target or depth write state.
                unsafe {
                    (l.vtbl().DiscardResource)(l.as_raw(), texture.resource.as_raw(), ptr::null())
                };
            }
        }
    }

    fn memory_barrier(list: &mut CommandList, before: BufferState, after: BufferState) {
        if let Some(l7) = &list.list7 {
            let barrier = D3D12_GLOBAL_BARRIER {
                SyncBefore: before.stage.bits(),
                SyncAfter: after.stage.bits(),
                AccessBefore: barrier_access(before.access),
                AccessAfter: barrier_access(after.access),
            };
            enhanced_flush(l7, D3D12_BARRIER_TYPE_GLOBAL)(&[barrier]);
            return;
        }
        // Legacy: a UAV barrier naming no resource orders every unordered-access and
        // acceleration structure access before it with every one after it.
        legacy_flush(&list.list)(&[uav_barrier(ptr::null_mut())]);
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
        // use, so a barrier without a prior access has nothing to do, except to activate a
        // placed buffer over aliased memory. Upload and readback buffers never leave their
        // initial state; acceleration structures never leave theirs and only order their
        // builds with UAV barriers.
        let mut flush = legacy_flush(&list.list);
        let mut batch = Batch::<_, 16>::new(EMPTY_TRANSITION);
        for b in barriers {
            let raw = &b.buffer.raw;
            let resource = raw.resource.as_raw();
            if b.before.access == Access::NONE {
                if raw.placed {
                    batch.push(aliasing_barrier(resource), &mut flush);
                }
                continue;
            }
            if raw.acceleration_structure {
                batch.push(uav_barrier(resource), &mut flush);
                continue;
            }
            if raw.host {
                continue;
            }
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
        if let (Some(handle), Some(d)) = (depth, desc.depth) {
            let (mut flags, mut depth_value, mut stencil_value) = (0, 0.0, 0);
            if let LoadOp::Clear(value) = d.load {
                flags |= D3D12_CLEAR_FLAG_DEPTH;
                depth_value = value;
            }
            if let (true, LoadOp::Clear(value)) = (d.view.raw.stencil, d.stencil_load) {
                flags |= D3D12_CLEAR_FLAG_STENCIL;
                stencil_value = value;
            }
            if flags != 0 {
                // SAFETY: valid interface and descriptor.
                unsafe {
                    (l.vtbl().ClearDepthStencilView)(
                        l.as_raw(),
                        handle,
                        flags,
                        depth_value,
                        stencil_value,
                        0,
                        ptr::null(),
                    )
                };
            }
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
            (l.vtbl().CopyTextureRegion)(
                l.as_raw(),
                &to,
                region.x,
                region.y,
                region.z,
                &from,
                ptr::null(),
            )
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
            front: region.z,
            right: region.x + region.width,
            bottom: region.y + region.height,
            back: region.z + region.depth,
        };
        let l = &list.list;
        // SAFETY: valid interface and locations; the region was checked by `api.rs`.
        unsafe { (l.vtbl().CopyTextureRegion)(l.as_raw(), &to, 0, 0, 0, &from, &area) };
    }

    fn copy_texture(
        list: &mut CommandList,
        src: &PublicTexture,
        dst: &PublicTexture,
        region: &TextureCopy,
    ) {
        let [x, y, z] = region.src_offset;
        let [w, h, d] = region.size;
        let area = D3D12_BOX {
            left: x,
            top: y,
            front: z,
            right: x + w,
            bottom: y + h,
            back: z + d,
        };
        let l = &list.list;
        // Depth-stencil copies cover both planes; multisampled copies need no box (whole
        // subresources, checked by `api.rs`).
        let whole = src.raw.sample_count > 1;
        for plane in 0..src.raw.planes {
            let from = subresource_location(
                src,
                subresource(&src.raw, region.src_mip, region.src_layer, plane),
            );
            let to = subresource_location(
                dst,
                subresource(&dst.raw, region.dst_mip, region.dst_layer, plane),
            );
            let [dx, dy, dz] = region.dst_offset;
            let area = match whole {
                true => ptr::null(),
                false => &raw const area,
            };
            // SAFETY: valid interface and locations; both boxes were checked by `api.rs`.
            unsafe { (l.vtbl().CopyTextureRegion)(l.as_raw(), &to, dx, dy, dz, &from, area) };
        }
    }

    fn resolve_texture(
        list: &mut CommandList,
        src: &PublicTexture,
        dst: &PublicTexture,
        r: &TextureResolve,
    ) {
        let format = resource::dxgi_format(r.format.unwrap_or(dst.desc.format));
        let l = &list.list;
        // SAFETY: valid interfaces; subresources and formats checked by `api.rs`.
        unsafe {
            (l.vtbl().ResolveSubresource)(
                l.as_raw(),
                dst.raw.resource.as_raw(),
                subresource(&dst.raw, r.dst_mip, r.dst_layer, 0),
                src.raw.resource.as_raw(),
                subresource(&src.raw, r.src_mip, r.src_layer, 0),
                format,
            )
        };
    }

    fn fill_buffer(list: &mut CommandList, buffer: &Buffer, offset: u64, size: u64, value: u32) {
        let Ok((gpu, cpu)) = list.clear_slots() else {
            marlin_diag::error!("fill_buffer: no descriptor slot left; the fill is skipped");
            return;
        };
        let raw = &buffer.raw;
        let d = &list.device;
        // The view must sit in both heaps: the GPU half for the clear itself, the CPU half for
        // drivers that clear with fixed-function hardware.
        let visible = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: list.transient[list.transient.len() - 2].handle().ptr,
        };
        resource::write_raw_buffer_view(d, raw, offset, size, true, visible);
        resource::write_raw_buffer_view(d, raw, offset, size, true, cpu);
        let l = &list.list;
        // SAFETY: valid interfaces; the views cover the checked range; the shader-visible heap
        // is bound.
        unsafe {
            (l.vtbl().ClearUnorderedAccessViewUint)(
                l.as_raw(),
                gpu,
                cpu,
                raw.resource.as_raw(),
                [value; 4].as_ptr(),
                0,
                ptr::null(),
            )
        };
    }

    fn clear_storage_texture(
        list: &mut CommandList,
        texture: &PublicTexture,
        range: &SubresourceRange,
        value: ClearColor,
    ) {
        let Ok((gpu, cpu)) = list.clear_slots() else {
            marlin_diag::error!("clear_storage_texture: no descriptor slot left; skipped");
            return;
        };
        let format = resource::dxgi_format(texture.desc.format);
        let array = texture.desc.dimension == TextureDimension::D2
            && (texture.desc.array_layers > 1 || range.base_layer > 0);
        let layers = (range.base_layer, range.layer_count);
        let visible = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: list.transient[list.transient.len() - 2].handle().ptr,
        };
        let d = &list.device;
        resource::write_texture_uav(d, texture, format, range.base_mip, layers, visible, array);
        resource::write_texture_uav(d, texture, format, range.base_mip, layers, cpu, array);
        let l = &list.list;
        let resource = texture.raw.resource.as_raw();
        // SAFETY: valid interfaces; the views cover the checked subresources; the
        // shader-visible heap is bound. Integer values are copied bit for bit.
        unsafe {
            match value {
                ClearColor::Float(v) => (l.vtbl().ClearUnorderedAccessViewFloat)(
                    l.as_raw(),
                    gpu,
                    cpu,
                    resource,
                    v.as_ptr(),
                    0,
                    ptr::null(),
                ),
                ClearColor::Uint(v) => (l.vtbl().ClearUnorderedAccessViewUint)(
                    l.as_raw(),
                    gpu,
                    cpu,
                    resource,
                    v.as_ptr(),
                    0,
                    ptr::null(),
                ),
                ClearColor::Sint(v) => (l.vtbl().ClearUnorderedAccessViewUint)(
                    l.as_raw(),
                    gpu,
                    cpu,
                    resource,
                    v.map(|c| c as u32).as_ptr(),
                    0,
                    ptr::null(),
                ),
            }
        };
    }

    fn write_marker(
        list: &mut CommandList,
        buffer: &Buffer,
        offset: u64,
        value: u32,
        point: MarkerPoint,
    ) {
        let parameter = D3D12_WRITEBUFFERIMMEDIATE_PARAMETER {
            Dest: buffer.raw.address + offset,
            Value: value,
        };
        let mode = match point {
            MarkerPoint::Reached => D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_IN,
            MarkerPoint::Completed => D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_OUT,
        };
        let l = &list.list;
        // SAFETY: valid interface; one parameter and mode; the address lies in the buffer.
        unsafe { (l.vtbl().WriteBufferImmediate)(l.as_raw(), 1, &parameter, &mode) };
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

    fn set_mesh_pipeline(list: &mut CommandList, pipeline: &MeshPipeline) {
        list.compute = false;
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

    fn set_viewports(list: &mut CommandList, viewports: &[Viewport]) {
        let mut raw = [const {
            D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: 0.0,
                Height: 0.0,
                MinDepth: 0.0,
                MaxDepth: 0.0,
            }
        }; MAX_VIEWPORTS];
        for (r, v) in raw.iter_mut().zip(viewports) {
            *r = D3D12_VIEWPORT {
                TopLeftX: v.x,
                TopLeftY: v.y,
                Width: v.width,
                Height: v.height,
                MinDepth: v.min_depth,
                MaxDepth: v.max_depth,
            };
        }
        let l = &list.list;
        // SAFETY: valid interface; `raw` holds `viewports.len()` entries (at most 16).
        unsafe { (l.vtbl().RSSetViewports)(l.as_raw(), viewports.len() as u32, raw.as_ptr()) };
    }

    fn set_scissors(list: &mut CommandList, rects: &[Rect]) {
        let mut raw = [const {
            D3D12_RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            }
        }; MAX_VIEWPORTS];
        for (r, rect) in raw.iter_mut().zip(rects) {
            *r = D3D12_RECT {
                left: rect.x,
                top: rect.y,
                right: rect.x.saturating_add_unsigned(rect.width),
                bottom: rect.y.saturating_add_unsigned(rect.height),
            };
        }
        let l = &list.list;
        // SAFETY: valid interface; `raw` holds `rects.len()` entries (at most 16).
        unsafe { (l.vtbl().RSSetScissorRects)(l.as_raw(), rects.len() as u32, raw.as_ptr()) };
    }

    fn set_stencil_reference(list: &mut CommandList, reference: u8) {
        let l = &list.list;
        // SAFETY: valid interface.
        unsafe { (l.vtbl().OMSetStencilRef)(l.as_raw(), reference as u32) };
    }

    fn set_blend_constants(list: &mut CommandList, constants: [f32; 4]) {
        let l = &list.list;
        // SAFETY: valid interface; four floats.
        unsafe { (l.vtbl().OMSetBlendFactor)(l.as_raw(), constants.as_ptr()) };
    }

    fn set_depth_bounds(list: &mut CommandList, min: f32, max: f32) {
        let l = &list.list;
        // SAFETY: valid interface; the device supports depth bounds (checked by `api.rs`).
        unsafe { (l.vtbl().OMSetDepthBounds)(l.as_raw(), min, max) };
    }

    fn set_shading_rate(
        list: &mut CommandList,
        rate: ShadingRate,
        combiners: [ShadingRateCombiner; 2],
    ) {
        let combiners = combiners.map(combiner);
        let l = &list.list;
        // SAFETY: valid interface; two combiners.
        unsafe { (l.vtbl().RSSetShadingRate)(l.as_raw(), shading_rate(rate), combiners.as_ptr()) };
    }

    fn set_shading_rate_image(list: &mut CommandList, image: Option<&PublicTexture>) {
        let l = &list.list;
        let raw = image.map_or(ptr::null_mut(), |t| t.raw.resource.as_raw());
        // SAFETY: valid interfaces; the image is an R8_UINT texture (checked by `api.rs`).
        unsafe { (l.vtbl().RSSetShadingRateImage)(l.as_raw(), raw) };
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

    fn dispatch_mesh(list: &mut CommandList, x: u32, y: u32, z: u32) {
        let l = &list.list;
        // SAFETY: valid interface; `api.rs` allows mesh dispatches on graphics lists only.
        unsafe { (l.vtbl().DispatchMesh)(l.as_raw(), x, y, z) };
    }

    fn draw_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
        list.execute_indirect(&list.bindless.signatures.draw, args);
    }

    fn draw_indexed_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
        list.execute_indirect(&list.bindless.signatures.draw_indexed, args);
    }

    fn dispatch_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
        list.execute_indirect(&list.bindless.signatures.dispatch, args);
    }

    fn dispatch_mesh_indirect(list: &mut CommandList, args: &IndirectArgs<'_>) {
        list.execute_indirect(&list.bindless.signatures.dispatch_mesh, args);
    }

    fn write_timestamp(list: &mut CommandList, pool: &QueryPool, index: u32) {
        let l = &list.list;
        // SAFETY: valid interfaces; `index` is inside the heap (checked by `api.rs`).
        unsafe {
            (l.vtbl().EndQuery)(
                l.as_raw(),
                pool.heap.as_raw(),
                D3D12_QUERY_TYPE_TIMESTAMP,
                index,
            )
        };
    }

    fn begin_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
        let l = &list.list;
        // SAFETY: valid interfaces; `index` is inside the heap (checked by `api.rs`).
        unsafe { (l.vtbl().BeginQuery)(l.as_raw(), pool.heap.as_raw(), pool.query_type, index) };
    }

    fn end_query(list: &mut CommandList, pool: &QueryPool, index: u32) {
        let l = &list.list;
        // SAFETY: as above.
        unsafe { (l.vtbl().EndQuery)(l.as_raw(), pool.heap.as_raw(), pool.query_type, index) };
    }

    fn resolve_queries(
        list: &mut CommandList,
        pool: &QueryPool,
        first: u32,
        count: u32,
        dst: &Buffer,
        offset: u64,
    ) {
        let l = &list.list;
        // SAFETY: valid interfaces; the query and buffer ranges were checked by `api.rs`.
        unsafe {
            (l.vtbl().ResolveQueryData)(
                l.as_raw(),
                pool.heap.as_raw(),
                pool.query_type,
                first,
                count,
                dst.raw.resource.as_raw(),
                offset,
            )
        };
    }

    fn set_predication(list: &mut CommandList, predicate: Option<(&Buffer, u64, PredicateSkip)>) {
        let (buffer, offset, op) = match predicate {
            Some((buffer, offset, skip)) => (
                buffer.raw.resource.as_raw(),
                offset,
                match skip {
                    PredicateSkip::IfZero => D3D12_PREDICATION_OP_EQUAL_ZERO,
                    PredicateSkip::IfNonZero => D3D12_PREDICATION_OP_NOT_EQUAL_ZERO,
                },
            ),
            None => (ptr::null_mut(), 0, D3D12_PREDICATION_OP_EQUAL_ZERO),
        };
        let l = &list.list;
        // SAFETY: valid interfaces; the predicate lies in the buffer (checked by `api.rs`).
        unsafe { (l.vtbl().SetPredication)(l.as_raw(), buffer, offset, op) };
    }

    fn build_acceleration_structure(
        list: &mut CommandList,
        build: &BuildAccelerationStructure<'_>,
    ) {
        let mut flags = build.flags.bits();
        if build.update.is_some() {
            flags |= D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE;
        }
        let mut geometries = core::mem::take(&mut list.geometries);
        let mut inputs = super::raytracing::inputs(&build.inputs, build.flags, &mut geometries);
        inputs.Flags = flags;
        let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: build.dst.raw.address,
            Inputs: inputs,
            SourceAccelerationStructureData: build.update.map_or(0, |s| s.raw.address),
            ScratchAccelerationStructureData: build.scratch.raw.address + build.scratch_offset,
        };
        let l = &list.list;
        // SAFETY: valid interface; `desc` points at `geometries`, alive for the call; the
        // ranges were checked by `api.rs`.
        unsafe {
            (l.vtbl().BuildRaytracingAccelerationStructure)(l.as_raw(), &desc, 0, ptr::null())
        };
        list.geometries = geometries;
    }

    fn copy_acceleration_structure(
        list: &mut CommandList,
        src: &AccelerationStructure,
        dst: &AccelerationStructure,
        mode: AccelerationStructureCopy,
    ) {
        let mode = match mode {
            AccelerationStructureCopy::Clone => {
                D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE
            }
            AccelerationStructureCopy::Compact => {
                D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT
            }
        };
        let l = &list.list;
        // SAFETY: valid interface and structures.
        unsafe {
            (l.vtbl().CopyRaytracingAccelerationStructure)(
                l.as_raw(),
                dst.raw.address,
                src.raw.address,
                mode,
            )
        };
    }

    fn write_compacted_sizes(
        list: &mut CommandList,
        structures: &[&AccelerationStructure],
        dst: &Buffer,
        offset: u64,
    ) {
        let addresses: Vec<u64> = structures.iter().map(|s| s.raw.address).collect();
        let desc = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
            DestBuffer: dst.raw.address + offset,
            InfoType: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE,
        };
        let l = &list.list;
        // SAFETY: valid interface; one size per structure lands in the checked range.
        unsafe {
            (l.vtbl().EmitRaytracingAccelerationStructurePostbuildInfo)(
                l.as_raw(),
                &desc,
                addresses.len() as u32,
                addresses.as_ptr(),
            )
        };
    }

    fn begin_event(list: &mut CommandList, name: &str) {
        list.event(name, list.list.vtbl().BeginEvent);
    }

    fn end_event(list: &mut CommandList) {
        let l = &list.list;
        // SAFETY: valid interface; `api.rs` callers pair events.
        unsafe { (l.vtbl().EndEvent)(l.as_raw()) };
    }

    fn set_marker(list: &mut CommandList, name: &str) {
        list.event(name, list.list.vtbl().SetMarker);
    }
}
