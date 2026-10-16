//! Command pools and lists, barriers and render passes.

use core::ffi::c_void;
use core::ptr;

use super::com::{self, *};
use super::ffi::*;
use super::{Backend, Device, check, error};
use crate::{
    Access, Layout, LoadOp, MAX_COLOR_ATTACHMENTS, RenderPassDesc, Result, SubresourceRange,
    TextureBarrier,
};

pub(crate) struct Texture {
    pub(super) resource: Com<ID3D12Resource>,
    pub(super) mip_levels: u32,
    pub(super) array_layers: u32,
}

pub(crate) struct RenderTargetView {
    pub(super) handle: D3D12_CPU_DESCRIPTOR_HANDLE,
}

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
        Ok(Self { list, list7 })
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

impl crate::contract::Commands for Backend {
    unsafe fn reset_pool(pool: &mut CommandPool) -> Result<()> {
        let a = &pool.allocator;
        // SAFETY: the caller guarantees the GPU finished the pool's lists.
        check(unsafe { (a.vtbl().Reset)(a.as_raw()) })
    }

    fn begin(list: &mut CommandList, pool: &mut CommandPool) -> Result<()> {
        let l = &list.list;
        // SAFETY: valid interfaces; D3D12 itself rejects resetting a list still recording.
        check(unsafe { (l.vtbl().Reset)(l.as_raw(), pool.allocator.as_raw(), ptr::null_mut()) })
    }

    fn end(list: &mut CommandList) -> Result<()> {
        // SAFETY: valid interface.
        check(unsafe { (list.list.vtbl().Close)(list.list.as_raw()) })
    }

    fn texture_barriers(list: &mut CommandList, barriers: &[TextureBarrier<'_>]) {
        if let Some(l7) = &list.list7 {
            let mut flush = |items: &[D3D12_TEXTURE_BARRIER]| {
                let group = D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_TEXTURE,
                    NumBarriers: items.len() as u32,
                    pBarriers: items.as_ptr().cast(),
                };
                // SAFETY: valid interface; `group` points at `items`, which outlives the call.
                unsafe { (l7.vtbl().Barrier)(l7.as_raw(), 1, &group) };
            };
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
            let mut batch = Batch {
                items: [EMPTY; 16],
                len: 0,
            };
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
        let l = &list.list;
        let mut flush = |items: &[D3D12_RESOURCE_BARRIER]| {
            // SAFETY: valid interface; `items` outlives the call.
            unsafe { (l.vtbl().ResourceBarrier)(l.as_raw(), items.len() as u32, items.as_ptr()) };
        };
        const EMPTY: D3D12_RESOURCE_BARRIER = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: 0,
            Transition: D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ptr::null_mut(),
                Subresource: 0,
                StateBefore: 0,
                StateAfter: 0,
            },
        };
        const UAV: i32 = 2;
        let mut batch = Batch {
            items: [EMPTY; 16],
            len: 0,
        };
        for b in barriers {
            let texture = &b.texture.raw;
            let resource = texture.resource.as_raw();
            let before = legacy_state(b.before.layout);
            let after = legacy_state(b.after.layout);
            if before == after {
                if before == D3D12_RESOURCE_STATE_UNORDERED_ACCESS {
                    let mut uav = EMPTY;
                    uav.Type = UAV;
                    // The UAV member shares the transition member's first field.
                    uav.Transition.pResource = resource;
                    batch.push(uav, &mut flush);
                }
                continue;
            }
            let mut transition = |subresource| {
                let mut t = EMPTY;
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

    fn begin_render_pass(list: &mut CommandList, desc: &RenderPassDesc<'_>) {
        assert!(desc.color.len() <= MAX_COLOR_ATTACHMENTS);
        let l = &list.list;
        let mut handles = [D3D12_CPU_DESCRIPTOR_HANDLE::default(); MAX_COLOR_ATTACHMENTS];
        for (h, a) in handles.iter_mut().zip(desc.color) {
            *h = a.view.raw.handle;
        }
        // SAFETY: valid interface; the handles are live descriptors (borrowed views).
        unsafe {
            (l.vtbl().OMSetRenderTargets)(
                l.as_raw(),
                desc.color.len() as u32,
                handles.as_ptr(),
                0,
                ptr::null(),
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
    }

    fn end_render_pass(_: &mut CommandList) {
        // Emulated passes have nothing to resolve or store.
    }
}
