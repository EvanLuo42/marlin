//! The frame context (0007 §7): runs compiled plans on the RHI. It owns what outlives one
//! frame's graph: the frame arenas, the plan cache and state table, a fence per queue, command
//! pools and lists and transient heaps per frame slot, the physical resources each plan places in them, the upload memory, the
//! views of imported objects, readbacks, and the queue of objects waiting for the GPU before they
//! are dropped.
//!
//! Each of the plan's segments is one command list, submitted in order with its waits and a
//! signal on its queue; the frame ends with a graphics submission that waits for the other
//! queues, so one graphics fence value covers the whole frame. Recording is on one thread for now;
//! parallel recording comes next (0007 §6).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::ptr;

use marlin_containers::{HashMap, HashMapExt, SmallVec};
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc as RhiBufferDesc, BufferTextureCopy, BufferUsage,
    BufferViewDesc, BufferViewKind, ColorAttachment, CommandList, CommandPool, DepthAttachment,
    DepthStencilView, DepthStencilViewDesc, Device, Fence, Heap, HeapDesc, HeapKind,
    MAX_COLOR_ATTACHMENTS, MemoryLocation, QueueType, Rect, RenderPassDesc, RenderTargetView,
    RenderTargetViewDesc, Result, ShaderView, SubmitDesc, Texture, TextureBarrier,
    TextureDimension, TextureViewDesc, TextureViewDimension, Viewport, aligned_row_pitch,
};
use marlin_std::mem::{Arena, FrameArenas};

use crate::access::{Access, PassKind, Use};
use crate::backend::{Backend as _, Rhi};
use crate::cache::{Lookup, PlanCache};
use crate::context::{NO_VIEW, PassContext, RhiParts};
use crate::desc::{BufferDesc, TextureDesc};
use crate::graph::{AccessRec, Builtin, Exec, Imported, Readback, RenderGraph, Res, Source};
use crate::plan::CompiledPlan;
use crate::retained::RetainedKind;
use crate::state::StateTable;
use crate::state::{buffer_object_usage, texture_object_usage};
use crate::sync;

/// Frames the CPU may record ahead of the GPU.
pub const FRAMES_IN_FLIGHT: usize = 2;

#[derive(Clone, Copy, Debug)]
pub struct FrameContextDesc {
    /// Address space each frame arena reserves (committed as used).
    pub arena_capacity: usize,
    /// Bytes of each upload memory chunk; larger uploads get a chunk of their own size.
    pub upload_chunk: u64,
    /// Run every pass on the graphics queue ([`RenderGraph::set_single_queue`]).
    pub single_queue: bool,
}

impl Default for FrameContextDesc {
    fn default() -> Self {
        Self {
            arena_capacity: 256 << 20,
            upload_chunk: 4 << 20,
            single_queue: false,
        }
    }
}

/// The frame [`FrameContext::execute`] declares. Its type also lets the declaring closure import
/// objects that live for `'env` (anything borrowed from outside the call): a `&'g FrameInfo<'env>`
/// argument implies `'env: 'g`, as `std::thread::scope` does.
#[derive(Debug)]
pub struct FrameInfo<'env> {
    pub frame: u64,
    _env: PhantomData<&'env ()>,
}

/// What the last [`FrameContext::execute`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameStats {
    /// The plan's topology key ([`PlanCache::plan`]).
    pub key: u64,
    /// Whether the plan came from the cache.
    pub lookup: Lookup,
    /// Passes recorded.
    pub passes: usize,
    /// RHI objects created for the plan: heaps, placed resources and views (upload views not
    /// counted). Zero in the steady state.
    pub created: u32,
}

/// Linear upload memory of one frame slot, in chunks kept across frames.
pub(crate) struct Upload {
    chunks: Vec<(Buffer, u64)>,
    current: usize,
    chunk_size: u64,
}

impl Upload {
    fn reset(&mut self) {
        for chunk in &mut self.chunks {
            chunk.1 = 0;
        }
        self.current = 0;
    }

    /// Copies `bytes` to an `align`ed offset with `size` bytes reserved there (at least the
    /// bytes); returns the buffer and the offset.
    pub(crate) fn write(
        &mut self,
        device: &Device,
        bytes: &[u8],
        align: u64,
        size: u64,
    ) -> (&Buffer, u64) {
        debug_assert!(size >= bytes.len() as u64);
        loop {
            if self.current == self.chunks.len() {
                let chunk = self.chunk_size.max(size.next_power_of_two());
                let buffer = device
                    .create_buffer(&RhiBufferDesc {
                        size: chunk,
                        usage: BufferUsage::UNIFORM
                            | BufferUsage::SHADER_READ
                            | BufferUsage::COPY_SRC,
                        memory: MemoryLocation::Upload,
                    })
                    .expect("upload memory");
                buffer.set_name("RenderGraphUpload");
                self.chunks.push((buffer, 0));
            }
            let (buffer, used) = &mut self.chunks[self.current];
            let offset = used.next_multiple_of(align);
            if offset + size <= buffer.size() {
                *used = offset + size;
                let base = buffer.mapped().expect("upload memory is mapped");
                // SAFETY: `offset + size <= buffer.size()` and the buffer is mapped for its life;
                // the GPU does not read this chunk before this frame is submitted (the slot's
                // previous frame finished before `reset`).
                unsafe {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        base.as_ptr().add(offset as usize),
                        bytes.len(),
                    );
                }
                return (&self.chunks[self.current].0, offset);
            }
            self.current += 1;
        }
    }
}

/// A plan's resources in one frame slot: placed transients and their views.
struct Physical {
    key: u64,
    textures: Vec<Option<Texture>>,
    buffers: Vec<Option<Buffer>>,
    /// By access index in the graph.
    views: Vec<Option<ShaderView>>,
    rtvs: Vec<Option<RenderTargetView>>,
    dsvs: Vec<Option<DepthStencilView>>,
}

struct Slot {
    /// Per queue (graphics, compute, copy), made when first needed.
    pools: [Option<CommandPool>; 3],
    /// Per queue, one per segment; reused frame after frame.
    lists: [Vec<CommandList>; 3],
    /// Fence value of the slot's last submission.
    value: u64,
    /// Buffers, textures, targets.
    heaps: [Option<Heap>; 3],
    physical: Vec<Physical>,
    upload: Upload,
}

/// A view of an imported object, by the object's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ViewSpec {
    Texture(TextureViewDesc),
    Buffer(BufferViewDesc),
    Rtv(RenderTargetViewDesc),
    Dsv(DepthStencilViewDesc),
}

enum View {
    Shader(ShaderView),
    Rtv(RenderTargetView),
    Dsv(DepthStencilView),
}

struct Cached {
    view: View,
    /// Last frame that used it.
    frame: u64,
}

struct PendingReadback {
    ticket: Readback,
    buffer: Buffer,
    /// Fence value after which it holds the data.
    value: u64,
    /// Texture rows: bytes per row, row pitch, rows.
    rows: Option<(u32, u32, u32)>,
}

/// Runs frames: declares each one's graph, compiles it (or finds it in the cache) and records,
/// submits and tracks it. See the module docs.
pub struct FrameContext {
    arenas: FrameArenas<FRAMES_IN_FLIGHT>,
    states: StateTable<Rhi>,
    cache: PlanCache,
    /// The graphics queue's: its value at the end of a frame covers the whole frame.
    fence: Fence,
    /// Value the next graphics submission signals.
    next_value: u64,
    /// The compute and copy queues', and the values their next submissions signal.
    async_fences: [Fence; 2],
    async_values: [u64; 2],
    slots: Vec<Slot>,
    import_views: HashMap<(u64, ViewSpec), Cached>,
    /// The objects of history and persistent resources, by key and slot.
    retained: HashMap<(u64, u8), RetainedObject>,
    /// Objects to drop once the fence reaches the value.
    retired: Vec<(u64, Box<dyn Send>)>,
    readbacks: Vec<PendingReadback>,
    stats: Option<FrameStats>,
    single_queue: bool,
}

const fn heap_slot(kind: HeapKind) -> usize {
    match kind {
        HeapKind::Buffers => 0,
        HeapKind::Textures => 1,
        HeapKind::Targets | HeapKind::Any => 2,
    }
}

const HEAP_KINDS: [HeapKind; 3] = [HeapKind::Buffers, HeapKind::Textures, HeapKind::Targets];

impl FrameContext {
    pub fn new(device: &Device, desc: &FrameContextDesc) -> Result<Self> {
        let mut slots = Vec::with_capacity(FRAMES_IN_FLIGHT);
        for _ in 0..FRAMES_IN_FLIGHT {
            slots.push(Slot {
                pools: [None, None, None],
                lists: Default::default(),
                value: 0,
                heaps: [None, None, None],
                physical: Vec::new(),
                upload: Upload {
                    chunks: Vec::new(),
                    current: 0,
                    chunk_size: desc.upload_chunk,
                },
            });
        }
        let arenas = FrameArenas::new(desc.arena_capacity)
            .map_err(|_| marlin_rhi::Error::from_kind(marlin_rhi::ErrorKind::OutOfMemory))?;
        Ok(Self {
            arenas,
            states: StateTable::new(),
            cache: PlanCache::new(),
            fence: device.create_fence(0)?,
            next_value: 1,
            async_fences: [device.create_fence(0)?, device.create_fence(0)?],
            async_values: [1, 1],
            slots,
            import_views: HashMap::new(),
            retained: HashMap::new(),
            retired: Vec::new(),
            readbacks: Vec::new(),
            stats: None,
            single_queue: desc.single_queue,
        })
    }

    /// The number of the next frame.
    pub fn frame(&self) -> u64 {
        self.states.frame()
    }

    pub fn states(&self) -> &StateTable<Rhi> {
        &self.states
    }

    pub fn cache(&self) -> &PlanCache {
        &self.cache
    }

    /// What the last frame did; `None` before the first.
    pub fn stats(&self) -> Option<FrameStats> {
        self.stats
    }

    /// Runs one frame: waits for the frame slot's previous use, has `declare` build the graph,
    /// compiles or looks up its plan, records and submits it. Returns what `declare` returned
    /// (readback tickets, typically). Swapchains present after this returns.
    pub fn execute<'env, R>(
        &mut self,
        device: &Device,
        declare: impl for<'g> FnOnce(&mut RenderGraph<'g>, &'g FrameInfo<'env>) -> R,
    ) -> Result<R> {
        let frame = self.states.frame();
        // Outlives the graph, which borrows it for `'g`.
        let info = FrameInfo {
            frame,
            _env: PhantomData,
        };
        let slot_index = (frame % FRAMES_IN_FLIGHT as u64) as usize;
        self.fence.wait(self.slots[slot_index].value, None)?;
        self.collect();
        let mut created = 0;

        let Self {
            arenas,
            states,
            cache,
            fence,
            next_value,
            async_fences,
            async_values,
            slots,
            import_views,
            retained,
            retired,
            readbacks,
            stats,
            single_queue,
        } = self;

        // The slot's previous frame finished: its pool, upload memory and arena are free.
        let slot = &mut slots[slot_index];
        for pool in slot.pools.iter_mut().flatten() {
            // SAFETY: the fence shows the GPU finished the slot's last frame, which covers every
            // queue's work recorded from the slot's pools.
            unsafe { pool.reset()? };
        }
        slot.upload.reset();
        // SAFETY: frame `frame - FRAMES_IN_FLIGHT`, the arena's last user, ended inside its own
        // `execute` (its graph and closures are gone), and only this frame allocates from it.
        let arena: &Arena = unsafe { arenas.begin(frame) };

        let mut g = RenderGraph::with_states(arena, states);
        g.set_single_queue(*single_queue);
        let out = declare(&mut g, &info);
        let (plan_index, lookup) = cache.ensure(&g, device)?;
        let plan = cache.get(plan_index);

        // Plans the cache let go: their resources go once their slot's GPU work is done.
        for slot in slots.iter_mut() {
            let mut i = 0;
            while i < slot.physical.len() {
                if cache.contains(slot.physical[i].key) {
                    i += 1;
                } else {
                    let p = slot.physical.swap_remove(i);
                    retired.push((slot.value, Box::new(p)));
                }
            }
        }
        let slot = &mut slots[slot_index];
        grow_heaps(slot, plan, device, &mut created)?;
        let physical = match slot.physical.iter().position(|p| p.key == plan.key) {
            Some(i) => i,
            None => {
                let p = create_physical(&g, plan, &slot.heaps, device, &mut created)?;
                slot.physical.push(p);
                slot.physical.len() - 1
            }
        };

        // Objects of history and persistent resources the plan uses.
        let last_submitted = *next_value - 1;
        for u in g.retained.iter() {
            let (planned_t, planned_b, used) = match u.res {
                Res::Texture(t) => {
                    let p = &plan.textures[t as usize];
                    (p.usage, Default::default(), p.lifetime.is_some())
                }
                Res::Buffer(b) => {
                    let p = &plan.buffers[b as usize];
                    (Default::default(), p.usage, p.lifetime.is_some())
                }
            };
            if !used {
                continue;
            }
            let (stored_t, stored_b) = if u.same {
                states.retained_usage(u.key, u.slot)
            } else {
                Default::default()
            };
            let object = match u.kind {
                RetainedKind::Texture(desc) => {
                    let rhi = desc.to_rhi(texture_object_usage(stored_t, planned_t));
                    match retained.get(&(u.key, u.slot)) {
                        Some(RetainedObject::Texture(o)) if *o.desc() == rhi => continue,
                        _ => {
                            let o = device.create_texture(&rhi)?;
                            o.set_name(u.name);
                            RetainedObject::Texture(o)
                        }
                    }
                }
                RetainedKind::Buffer(desc) => {
                    let rhi = desc.to_rhi(buffer_object_usage(stored_b, planned_b));
                    match retained.get(&(u.key, u.slot)) {
                        Some(RetainedObject::Buffer(o)) if *o.desc() == rhi => continue,
                        _ => {
                            let o = device.create_buffer(&rhi)?;
                            o.set_name(u.name);
                            RetainedObject::Buffer(o)
                        }
                    }
                }
            };
            created += 1;
            if let Some(old) = retained.insert((u.key, u.slot), object) {
                retire_retained(old, import_views, retired, last_submitted);
            }
        }

        // Per-frame tables: objects, shapes, view indices.
        let physical = &slot.physical[physical];
        let retained_object = |source: &Source<()>| -> Option<&RetainedObject> {
            let Source::Retained(i) = source else {
                return None;
            };
            let u = &g.retained[*i as usize];
            retained.get(&(u.key, u.slot))
        };
        let textures: Vec<Option<&Texture>, &Arena> = {
            let mut v = Vec::with_capacity_in(g.textures.len(), arena);
            for (t, node) in g.textures.iter().enumerate() {
                v.push(match &node.import {
                    Some(Imported {
                        object: Source::Object(o),
                        ..
                    }) => Some(*o),
                    Some(Imported {
                        object: Source::Retained(i),
                        ..
                    }) => match retained_object(&Source::Retained(*i)) {
                        Some(RetainedObject::Texture(o)) => Some(o),
                        _ => None,
                    },
                    None => physical.textures[t].as_ref(),
                });
            }
            v
        };
        let buffers: Vec<Option<&Buffer>, &Arena> = {
            let mut v = Vec::with_capacity_in(g.buffers.len(), arena);
            for (b, node) in g.buffers.iter().enumerate() {
                v.push(match &node.import {
                    Some(Imported {
                        object: Source::Object(o),
                        ..
                    }) => Some(*o),
                    Some(Imported {
                        object: Source::Retained(i),
                        ..
                    }) => match retained_object(&Source::Retained(*i)) {
                        Some(RetainedObject::Buffer(o)) => Some(o),
                        _ => None,
                    },
                    None => physical.buffers[b].as_ref(),
                });
            }
            v
        };

        // Views of imported and graph-owned objects.
        for pp in &plan.passes {
            let pass = &g.passes[pp.node as usize];
            for i in pass.accesses.start as usize..pass.accesses.end as usize {
                let rec = &g.accesses[i];
                let Some((object, spec)) = import_spec(&g, rec, pass, &textures, &buffers) else {
                    continue;
                };
                match import_views.get_mut(&(object, spec)) {
                    Some(cached) => cached.frame = frame,
                    None => {
                        created += 1;
                        let target = match rec.res {
                            Res::Texture(t) => Object::Texture(textures[t as usize].expect("used")),
                            Res::Buffer(b) => Object::Buffer(buffers[b as usize].expect("used")),
                        };
                        let view = create_view(device, target, spec)?;
                        import_views.insert((object, spec), Cached { view, frame });
                    }
                }
            }
        }
        let shapes: Vec<(u32, u32), &Arena> = {
            let mut v = Vec::with_capacity_in(g.textures.len(), arena);
            v.extend(
                g.textures
                    .iter()
                    .map(|t| (t.desc.mip_levels, t.desc.layers())),
            );
            v
        };
        let mut views: Vec<u32, &Arena> = Vec::with_capacity_in(g.accesses.len(), arena);
        views.resize(g.accesses.len(), NO_VIEW);
        for pp in &plan.passes {
            let pass = &g.passes[pp.node as usize];
            for i in pass.accesses.start as usize..pass.accesses.end as usize {
                let rec = &g.accesses[i];
                views[i] = if is_transient(&g, rec.res) {
                    physical.views[i]
                        .as_ref()
                        .map_or(NO_VIEW, ShaderView::index)
                } else {
                    match import_spec(&g, rec, pass, &textures, &buffers) {
                        Some(key @ (_, ViewSpec::Texture(_) | ViewSpec::Buffer(_))) => {
                            match &import_views[&key].view {
                                View::Shader(v) => v.index(),
                                _ => unreachable!("shader specs make shader views"),
                            }
                        }
                        _ => NO_VIEW,
                    }
                };
            }
        }

        // Readback destinations.
        let first_readback = readbacks.len();
        for pp in &plan.passes {
            let pass = &g.passes[pp.node as usize];
            let rec = &g.accesses[pass.accesses.start as usize];
            let (size, rows, index) = match pass.builtin {
                Some(Builtin::ReadbackBuffer(index)) => (rec.bytes.1, None, index),
                Some(Builtin::ReadbackTexture(index)) => {
                    let Res::Texture(t) = rec.res else {
                        unreachable!("a texture readback reads a texture")
                    };
                    let desc = &g.textures[t as usize].desc;
                    let copy = texture_copy(desc, rec);
                    let block = desc.format.block_extent();
                    let rows = copy.height.div_ceil(block) * copy.depth;
                    let row_bytes = copy.width.div_ceil(block) * desc.format.copy_block_size();
                    (
                        u64::from(copy.bytes_per_row) * u64::from(rows),
                        Some((row_bytes, copy.bytes_per_row, rows)),
                        index,
                    )
                }
                _ => continue,
            };
            let buffer = device.create_buffer(&RhiBufferDesc {
                size: size.max(4),
                usage: BufferUsage::COPY_DST,
                memory: MemoryLocation::Readback,
            })?;
            buffer.set_name("RenderGraphReadback");
            readbacks.push(PendingReadback {
                ticket: Readback { frame, index },
                buffer,
                value: *next_value,
                rows,
            });
        }

        // Record.
        let mut execs: Vec<Option<Exec<'_, Rhi>>, &Arena> =
            Vec::with_capacity_in(g.passes.len(), arena);
        execs.extend(g.passes.iter_mut().map(|p| p.exec.take()));
        let mut retire_now: Vec<Box<dyn Send>> = Vec::new();
        let (pools, lists, upload) = (&mut slot.pools, &mut slot.lists, &mut slot.upload);
        let mut used_lists = [0usize; 3];
        // Each segment's signal value on its queue, for the waits on it.
        let mut segment_values: Vec<u64, &Arena> =
            Vec::with_capacity_in(plan.segments.len(), arena);
        let mut waited_last_frame = [false; 3];
        let previous_frame_end = *next_value - 1;
        let mut last_async = [0u64; 3];
        for (si, segment) in plan.segments.iter().enumerate() {
            let q = sync::queue_index(segment.queue) as usize;
            if pools[q].is_none() {
                pools[q] = Some(device.create_command_pool_for(segment.queue)?);
            }
            let pool = pools[q].as_mut().expect("made above");
            if lists[q].len() == used_lists[q] {
                lists[q].push(device.create_command_list(pool)?);
            }
            let list = &mut lists[q][used_lists[q]];
            used_lists[q] += 1;
            list.begin(pool)?;
            for &pos in plan.segment_passes(si) {
                let pos = pos as usize;
                let pp = &plan.passes[pos];
                let pass = &g.passes[pp.node as usize];
                list.begin_event(&pp.name);
                record_barriers(list, plan, pos, &textures, &buffers);
                let recs = &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize];
                match pass.builtin {
                    Some(Builtin::Present) => {}
                    Some(Builtin::ReadbackBuffer(index) | Builtin::ReadbackTexture(index)) => {
                        let dst = &readbacks[first_readback..]
                            .iter()
                            .find(|r| r.ticket.index == index)
                            .expect("made above")
                            .buffer;
                        let rec = &recs[0];
                        match rec.res {
                            Res::Buffer(b) => {
                                let src = buffers[b as usize].expect("a used buffer");
                                list.copy_buffer(src, rec.bytes.0, dst, 0, rec.bytes.1);
                            }
                            Res::Texture(t) => {
                                let src = textures[t as usize].expect("a used texture");
                                let copy = texture_copy(&g.textures[t as usize].desc, rec);
                                list.copy_texture_to_buffer(src, dst, &copy);
                            }
                        }
                    }
                    None => {
                        let raster = pass.kind == PassKind::Raster;
                        if raster {
                            begin_render_pass(
                                list,
                                &g,
                                pass,
                                physical,
                                &*import_views,
                                &textures,
                                &buffers,
                            );
                        }
                        if let Some(exec) = execs[pp.node as usize].take() {
                            let views =
                                &views[pass.accesses.start as usize..pass.accesses.end as usize];
                            let mut ctx = PassContext {
                                parts: RhiParts {
                                    list: &mut *list,
                                    device,
                                    pass: pass.name,
                                    recs,
                                    views,
                                    textures: &textures,
                                    buffers: &buffers,
                                    shapes: &shapes,
                                    upload: &mut *upload,
                                    retire: &mut retire_now,
                                },
                            };
                            exec(&mut ctx);
                        }
                        if raster {
                            list.end_render_pass();
                        }
                    }
                }
                list.end_event();
            }
            list.end()?;
            let mut waits: SmallVec<(&Fence, u64), 4> = segment
                .waits
                .iter()
                .map(|&w| {
                    let wq = sync::queue_index(plan.segments[w as usize].queue) as usize;
                    (
                        queue_fence(fence, async_fences, wq),
                        segment_values[w as usize],
                    )
                })
                .collect();
            // A queue other than graphics starts after the previous frame (the frame's last
            // graphics submission waited for all its queues).
            if q != 0 && !waited_last_frame[q] && previous_frame_end > 0 {
                waits.push((&*fence, previous_frame_end));
            }
            waited_last_frame[q] = true;
            let (signal_fence, value) = if q == 0 {
                let v = *next_value;
                *next_value += 1;
                (&*fence, v)
            } else {
                let v = async_values[q - 1];
                async_values[q - 1] += 1;
                last_async[q] = v;
                (&async_fences[q - 1], v)
            };
            // SAFETY: what the lists use stays alive until the frame's graphics fence value, which
            // comes after every list of the frame (the last graphics submission waits for the
            // other queues): transients and their views in the slot's physical set (dropped
            // only after this slot's fence, or through `retired`), imported objects' views in
            // the cache (retired the same way), upload views in `retired` below, and imported
            // objects by the caller's contract (forget them before dropping them). Pools are
            // reset only after the slot's fence. Every wait is on a value submitted before.
            unsafe {
                device.queue_for(segment.queue).submit_with(&SubmitDesc {
                    waits: &waits,
                    lists: &[&*list],
                    signals: &[(signal_fence, value)],
                })?
            };
            segment_values.push(value);
        }
        // The frame ends on the graphics queue, after the other queues' work: one value covers
        // the whole frame.
        let async_waits: SmallVec<(&Fence, u64), 2> = (1..3)
            .filter(|&q| last_async[q] != 0)
            .map(|q| (&async_fences[q - 1], last_async[q]))
            .collect();
        let graphics_last = plan
            .segments
            .iter()
            .rposition(|s| s.queue == QueueType::Graphics);
        let value = if async_waits.is_empty() && graphics_last.is_some() {
            *next_value - 1
        } else {
            let v = *next_value;
            *next_value += 1;
            // SAFETY: an empty submission; the waits are on values submitted above.
            unsafe {
                device.queue().submit_with(&SubmitDesc {
                    waits: &async_waits,
                    lists: &[],
                    signals: &[(&*fence, v)],
                })?
            };
            v
        };
        slot.value = value;
        // Readbacks hold their data once the whole frame is done.
        for r in &mut readbacks[first_readback..] {
            r.value = value;
        }
        for object in retire_now {
            retired.push((value, object));
        }

        *stats = Some(FrameStats {
            key: plan.key,
            lookup,
            passes: plan.passes.len(),
            created,
        });
        let frame_states = g.frame_states(plan);
        drop(execs);
        drop(g);
        states.end_frame(frame_states);

        // Objects of graph-owned resources the table let go, or whose description changed.
        let orphans: Vec<(u64, u8)> = retained
            .iter()
            .filter(|((key, _), object)| match (states.retained(*key), object) {
                (Some(e), RetainedObject::Texture(o)) => {
                    e.kind != RetainedKind::Texture(TextureDesc::from_rhi(o.desc()))
                }
                (Some(e), RetainedObject::Buffer(o)) => !matches!(
                    e.kind,
                    RetainedKind::Buffer(d) if d.size == o.desc().size
                ),
                (None, _) => true,
            })
            .map(|(k, _)| *k)
            .collect();
        for key in orphans {
            let object = retained.remove(&key).expect("listed above");
            retire_retained(object, import_views, retired, value);
        }

        // Views of imports no frame in flight uses any more.
        let stale: Vec<(u64, ViewSpec)> = import_views
            .iter()
            .filter(|(_, c)| frame - c.frame > FRAMES_IN_FLIGHT as u64)
            .map(|(k, _)| *k)
            .collect();
        for key in stale {
            let cached = import_views.remove(&key).expect("listed above");
            retired.push((value, Box::new(cached.view)));
        }
        Ok(out)
    }

    /// The data of a readback, once the GPU finished its frame; `None` before. Each readback
    /// is taken once. Texture rows come tightly packed.
    pub fn readback(&mut self, ticket: Readback) -> Option<Vec<u8>> {
        let i = self
            .readbacks
            .iter()
            .position(|r| r.ticket == ticket)
            .expect("a readback of this context, not taken yet");
        if self.fence.completed_value() < self.readbacks[i].value {
            return None;
        }
        let r = self.readbacks.swap_remove(i);
        let base = r.buffer.mapped().expect("readback memory is mapped");
        let size = r.buffer.size() as usize;
        // SAFETY: the buffer is mapped for its life, `size` bytes long, and the GPU finished
        // writing it (the fence passed its value).
        let bytes = unsafe { core::slice::from_raw_parts(base.as_ptr(), size) };
        Some(match r.rows {
            None => bytes.to_vec(),
            Some((row_bytes, pitch, rows)) => {
                let mut out = Vec::with_capacity(row_bytes as usize * rows as usize);
                for row in 0..rows as usize {
                    let start = row * pitch as usize;
                    out.extend_from_slice(&bytes[start..start + row_bytes as usize]);
                }
                out
            }
        })
    }

    /// As [`readback`](Self::readback), waiting for the GPU.
    pub fn wait_readback(&mut self, ticket: Readback) -> Result<Vec<u8>> {
        let value = self
            .readbacks
            .iter()
            .find(|r| r.ticket == ticket)
            .expect("a readback of this context, not taken yet")
            .value;
        self.fence.wait(value, None)?;
        Ok(self.readback(ticket).expect("the frame finished"))
    }

    /// Forgets an imported texture: its state and views. Call it before dropping the texture
    /// (a swapchain's images before a resize).
    pub fn forget_texture(&mut self, texture: &Texture) {
        self.states.forget_texture(texture);
        self.forget(Rhi::texture_key(texture));
    }

    pub fn forget_buffer(&mut self, buffer: &Buffer) {
        self.states.forget_buffer(buffer);
        self.forget(Rhi::buffer_key(buffer));
    }

    fn forget(&mut self, object: u64) {
        let last = self.next_value - 1;
        retire_views(&mut self.import_views, &mut self.retired, object, last);
    }

    /// Blocks until every submitted frame has finished, and drops what they retired.
    pub fn wait_idle(&mut self) -> Result<()> {
        self.fence.wait(self.next_value - 1, None)?;
        self.collect();
        Ok(())
    }

    /// Drops retired objects the GPU is done with.
    fn collect(&mut self) {
        let done = self.fence.completed_value();
        self.retired.retain(|(value, _)| *value > done);
    }
}

impl Drop for FrameContext {
    fn drop(&mut self) {
        // Everything here may be in use by submitted frames.
        let _ = self.fence.wait(self.next_value - 1, None);
    }
}

fn is_transient(g: &RenderGraph<'_>, res: Res) -> bool {
    match res {
        Res::Texture(t) => g.textures[t as usize].import.is_none(),
        Res::Buffer(b) => g.buffers[b as usize].import.is_none(),
    }
}

enum Object<'a> {
    Texture(&'a Texture),
    Buffer(&'a Buffer),
}

/// The object of a history or persistent resource.
enum RetainedObject {
    Texture(Texture),
    Buffer(Buffer),
}

/// Retires a graph-owned object and the cached views of it.
fn retire_retained(
    object: RetainedObject,
    import_views: &mut HashMap<(u64, ViewSpec), Cached>,
    retired: &mut Vec<(u64, Box<dyn Send>)>,
    value: u64,
) {
    let key = match &object {
        RetainedObject::Texture(t) => Rhi::texture_key(t),
        RetainedObject::Buffer(b) => Rhi::buffer_key(b),
    };
    retire_views(import_views, retired, key, value);
    retired.push((value, Box::new(object)));
}

/// Retires the cached views of the object with native handle `object`.
fn retire_views(
    import_views: &mut HashMap<(u64, ViewSpec), Cached>,
    retired: &mut Vec<(u64, Box<dyn Send>)>,
    object: u64,
    value: u64,
) {
    let keys: Vec<(u64, ViewSpec)> = import_views
        .keys()
        .filter(|k| k.0 == object)
        .copied()
        .collect();
    for key in keys {
        let cached = import_views.remove(&key).expect("listed above");
        retired.push((value, Box::new(cached.view)));
    }
}

/// The view an access of an imported or graph-owned object needs, by the object's key.
/// Transients have none here: their views live with the plan.
fn import_spec(
    g: &RenderGraph<'_>,
    rec: &AccessRec,
    pass: &crate::graph::PassNode<'_, Rhi>,
    textures: &[Option<&Texture>],
    buffers: &[Option<&Buffer>],
) -> Option<(u64, ViewSpec)> {
    if is_transient(g, rec.res) {
        return None;
    }
    let spec = view_spec(g, rec, pass)?;
    let key = match rec.res {
        Res::Texture(t) => Rhi::texture_key(textures[t as usize]?),
        Res::Buffer(b) => Rhi::buffer_key(buffers[b as usize]?),
    };
    Some((key, spec))
}

/// The view an access needs, if any: shader views of shader reads and writes, uniform views,
/// attachment views.
fn view_spec(
    g: &RenderGraph<'_>,
    rec: &AccessRec,
    pass: &crate::graph::PassNode<'_, Rhi>,
) -> Option<ViewSpec> {
    match rec.res {
        Res::Texture(t) => {
            let desc = &g.textures[t as usize].desc;
            let r = rec.range;
            match rec.usage {
                Use::Shader(
                    Access::ComputeRead | Access::VertexRead | Access::PixelRead | Access::MeshRead,
                ) => Some(ViewSpec::Texture(TextureViewDesc {
                    writable: false,
                    dimension: texture_dimension(desc, r.base_layer, r.layer_count),
                    format: None,
                    range: r.to_rhi(),
                })),
                Use::Shader(Access::ComputeWrite | Access::PixelWrite) if r.mip_count == 1 => {
                    Some(ViewSpec::Texture(TextureViewDesc {
                        writable: true,
                        dimension: texture_dimension(desc, r.base_layer, r.layer_count),
                        format: None,
                        range: r.to_rhi(),
                    }))
                }
                Use::Color => Some(ViewSpec::Rtv(RenderTargetViewDesc {
                    format: None,
                    mip: r.base_mip,
                    layer: r.base_layer,
                })),
                Use::DepthWrite | Use::DepthRead => Some(ViewSpec::Dsv(DepthStencilViewDesc {
                    mip: r.base_mip,
                    layer: r.base_layer,
                    read_only: pass.depth.is_some_and(|d| d.read_only),
                })),
                _ => None,
            }
        }
        Res::Buffer(b) => {
            let desc: &BufferDesc = &g.buffers[b as usize].desc;
            let (offset, size) = rec.bytes;
            let kind = match desc.stride {
                Some(stride) => BufferViewKind::Structured { stride },
                None => BufferViewKind::Raw,
            };
            let (kind, writable) = match rec.usage {
                Use::Shader(
                    Access::ComputeRead | Access::VertexRead | Access::PixelRead | Access::MeshRead,
                ) => (kind, false),
                Use::Shader(Access::ComputeWrite | Access::PixelWrite) => (kind, true),
                Use::Shader(Access::Uniform) => (BufferViewKind::Uniform, false),
                _ => return None,
            };
            Some(ViewSpec::Buffer(BufferViewDesc {
                kind,
                writable,
                offset,
                size,
            }))
        }
    }
}

/// `D2` for one layer of a non-array texture, `D2Array` otherwise, `D3` for volumes.
fn texture_dimension(desc: &TextureDesc, base_layer: u32, layers: u32) -> TextureViewDimension {
    match desc.dimension {
        TextureDimension::D3 => TextureViewDimension::D3,
        TextureDimension::D2 if desc.array_layers == 1 && base_layer == 0 && layers == 1 => {
            TextureViewDimension::D2
        }
        TextureDimension::D2 => TextureViewDimension::D2Array,
    }
}

fn create_view(device: &Device, object: Object<'_>, spec: ViewSpec) -> Result<View> {
    Ok(match (object, spec) {
        (Object::Texture(t), ViewSpec::Texture(d)) => {
            View::Shader(device.create_texture_view(t, &d)?)
        }
        (Object::Buffer(b), ViewSpec::Buffer(d)) => View::Shader(device.create_buffer_view(b, &d)?),
        (Object::Texture(t), ViewSpec::Rtv(d)) => {
            View::Rtv(device.create_render_target_view(t, &d)?)
        }
        (Object::Texture(t), ViewSpec::Dsv(d)) => {
            View::Dsv(device.create_depth_stencil_view(t, &d)?)
        }
        _ => unreachable!("specs follow the resource kind"),
    })
}

/// Grows the slot's heaps to what the plan needs. A grown heap takes the slot's physical sets
/// with it: they were placed in the old one. The slot's GPU work is done, so they go now.
fn grow_heaps(
    slot: &mut Slot,
    plan: &CompiledPlan,
    device: &Device,
    created: &mut u32,
) -> Result<()> {
    for kind in HEAP_KINDS {
        let need = plan.heap_size(kind);
        let i = heap_slot(kind);
        let have = slot.heaps[i].as_ref().map_or(0, |h| h.desc().size);
        if need <= have {
            continue;
        }
        slot.physical.clear();
        // Some headroom, so a slightly larger frame does not rebuild everything.
        let size = (need + need / 4).next_multiple_of(1 << 20);
        slot.heaps[i] = None;
        slot.heaps[i] = Some(device.create_heap(&HeapDesc {
            size,
            memory: MemoryLocation::GpuOnly,
            kind,
        })?);
        *created += 1;
    }
    Ok(())
}

fn create_physical(
    g: &RenderGraph<'_>,
    plan: &CompiledPlan,
    heaps: &[Option<Heap>; 3],
    device: &Device,
    created: &mut u32,
) -> Result<Physical> {
    let heap = |kind: HeapKind| heaps[heap_slot(kind)].as_ref().expect("grown to the plan");
    let mut textures = Vec::with_capacity(g.textures.len());
    for (t, node) in g.textures.iter().enumerate() {
        let planned = &plan.textures[t];
        textures.push(match (&node.import, planned.placement) {
            (None, Some(p)) => {
                let texture = device.create_placed_texture(
                    heap(p.heap),
                    p.offset,
                    &node.desc.to_rhi(planned.usage),
                )?;
                texture.set_name(node.name);
                *created += 1;
                Some(texture)
            }
            _ => None,
        });
    }
    let mut buffers = Vec::with_capacity(g.buffers.len());
    for (b, node) in g.buffers.iter().enumerate() {
        let planned = &plan.buffers[b];
        buffers.push(match (&node.import, planned.placement) {
            (None, Some(p)) => {
                let buffer = device.create_placed_buffer(
                    heap(p.heap),
                    p.offset,
                    &node.desc.to_rhi(planned.usage),
                )?;
                buffer.set_name(node.name);
                *created += 1;
                Some(buffer)
            }
            _ => None,
        });
    }
    let n = g.accesses.len();
    let mut views: Vec<Option<ShaderView>> = (0..n).map(|_| None).collect();
    let mut rtvs: Vec<Option<RenderTargetView>> = (0..n).map(|_| None).collect();
    let mut dsvs: Vec<Option<DepthStencilView>> = (0..n).map(|_| None).collect();
    for pp in &plan.passes {
        let pass = &g.passes[pp.node as usize];
        for i in pass.accesses.start as usize..pass.accesses.end as usize {
            let rec = &g.accesses[i];
            if !is_transient(g, rec.res) {
                continue;
            }
            let Some(spec) = view_spec(g, rec, pass) else {
                continue;
            };
            let object = match rec.res {
                Res::Texture(t) => Object::Texture(textures[t as usize].as_ref().expect("used")),
                Res::Buffer(b) => Object::Buffer(buffers[b as usize].as_ref().expect("used")),
            };
            *created += 1;
            match create_view(device, object, spec)? {
                View::Shader(v) => views[i] = Some(v),
                View::Rtv(v) => rtvs[i] = Some(v),
                View::Dsv(v) => dsvs[i] = Some(v),
            }
        }
    }
    Ok(Physical {
        key: plan.key,
        textures,
        buffers,
        views,
        rtvs,
        dsvs,
    })
}

/// The copy of one mip and layer an access covers into rows of a buffer.
fn texture_copy(desc: &TextureDesc, rec: &AccessRec) -> BufferTextureCopy {
    let rhi = desc.to_rhi(Default::default());
    let (width, height) = rhi.mip_size(rec.range.base_mip);
    BufferTextureCopy {
        buffer_offset: 0,
        bytes_per_row: aligned_row_pitch(desc.format, width),
        mip: rec.range.base_mip,
        layer: rec.range.base_layer,
        x: 0,
        y: 0,
        z: 0,
        width,
        height,
        depth: rhi.mip_depth(rec.range.base_mip),
    }
}

fn record_barriers(
    list: &mut CommandList,
    plan: &CompiledPlan,
    pos: usize,
    textures: &[Option<&Texture>],
    buffers: &[Option<&Buffer>],
) {
    let tb: Vec<TextureBarrier<'_>> = plan
        .texture_barriers(pos)
        .iter()
        .map(|t| TextureBarrier {
            texture: textures[t.texture as usize].expect("a used texture"),
            before: t.before,
            after: t.after,
            range: t.range,
            undefined_from: t.undefined_from,
        })
        .collect();
    if !tb.is_empty() {
        list.texture_barriers(&tb);
    }
    let bb: Vec<BufferBarrier<'_>> = plan
        .buffer_barriers(pos)
        .iter()
        .map(|b| BufferBarrier {
            buffer: buffers[b.buffer as usize].expect("a used buffer"),
            before: b.before,
            after: b.after,
        })
        .collect();
    if !bb.is_empty() {
        list.buffer_barriers(&bb);
    }
}

/// Binds a raster pass's attachments and sets the viewport and scissor to the first one.
fn begin_render_pass(
    list: &mut CommandList,
    g: &RenderGraph<'_>,
    pass: &crate::graph::PassNode<'_, Rhi>,
    physical: &Physical,
    import_views: &HashMap<(u64, ViewSpec), Cached>,
    textures: &[Option<&Texture>],
    buffers: &[Option<&Buffer>],
) {
    let attachment_view = |access: u32| -> &View {
        let rec = &g.accesses[access as usize];
        if is_transient(g, rec.res) {
            unreachable!("transient attachments are looked up directly")
        }
        let key = import_spec(g, rec, pass, textures, buffers).expect("attachments have views");
        &import_views[&key].view
    };
    let mut colors: marlin_containers::ArrayVec<ColorAttachment<'_>, MAX_COLOR_ATTACHMENTS> =
        marlin_containers::ArrayVec::new();
    for c in &pass.colors {
        let rec = &g.accesses[c.access as usize];
        let view = if is_transient(g, rec.res) {
            physical.rtvs[c.access as usize]
                .as_ref()
                .expect("made with the plan")
        } else {
            match attachment_view(c.access) {
                View::Rtv(v) => v,
                _ => unreachable!("color attachments have render target views"),
            }
        };
        let _ = colors.try_push(ColorAttachment {
            view,
            load: c.load,
            store: c.store,
        });
    }
    let depth = pass.depth.as_ref().map(|d| {
        let rec = &g.accesses[d.access as usize];
        let view = if is_transient(g, rec.res) {
            physical.dsvs[d.access as usize]
                .as_ref()
                .expect("made with the plan")
        } else {
            match attachment_view(d.access) {
                View::Dsv(v) => v,
                _ => unreachable!("depth attachments have depth views"),
            }
        };
        DepthAttachment {
            view,
            load: d.load,
            store: d.store,
            stencil_load: d.stencil_load,
            stencil_store: d.stencil_store,
        }
    });
    list.begin_render_pass(&RenderPassDesc {
        color: &colors,
        depth,
    });
    let first = pass
        .colors
        .first()
        .map(|c| (c.texture, c.mip))
        .or(pass.depth.map(|d| (d.texture, d.mip)));
    if let Some((t, mip)) = first {
        let desc = textures[t as usize].expect("an attachment").desc();
        let (w, h) = desc.mip_size(mip);
        list.set_viewport(&Viewport::full(w, h));
        list.set_scissor(&Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        });
    }
}

/// Queue `q`'s fence: graphics, compute or copy.
fn queue_fence<'f>(graphics: &'f Fence, others: &'f [Fence; 2], q: usize) -> &'f Fence {
    match q {
        0 => graphics,
        q => &others[q - 1],
    }
}
