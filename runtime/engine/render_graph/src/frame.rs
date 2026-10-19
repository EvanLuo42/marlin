//! The frame context (0007 §7): runs compiled plans on the RHI. It owns what outlives one
//! frame's graph: the frame arenas, the plan cache and state table, a fence per queue, command
//! pools and lists and transient heaps per frame slot, the physical resources each plan places in them, the upload memory, the
//! views of imported objects, readbacks, and the queue of objects waiting for the GPU before they
//! are dropped.
//!
//! Each of the plan's segments is one command list, submitted in order with its waits and a
//! signal on its queue; the frame ends with a graphics submission that waits for the other
//! queues, so one graphics fence value covers the whole frame. Each segment is recorded as chunks
//! of about [`CHUNK_COST_US`], as jobs when the frame runs on a job system (0007 §6).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::ptr;

use marlin_containers::{HashMap, HashMapExt, SmallVec};
use marlin_hash::fast_hash;
use marlin_jobs::JobSystem;
use marlin_jobs::sync::Mutex;
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc as RhiBufferDesc, BufferTextureCopy, BufferUsage,
    BufferViewDesc, BufferViewKind, ColorAttachment, CommandList, DepthAttachment,
    DepthStencilView, DepthStencilViewDesc, Device, Fence, Heap, HeapDesc, HeapKind,
    MAX_COLOR_ATTACHMENTS, MemoryLocation, QueryKind, QueryPool, QueryPoolDesc, QueueType,
    RenderTargetView, RenderTargetViewDesc, Result, ShaderView, SubmitDesc, Texture,
    TextureBarrier, TextureCopy, TextureDimension, TextureViewDesc, TextureViewDimension,
    aligned_row_pitch,
};
use marlin_std::mem::{Arena, FrameArenas};
use marlin_std::time::Instant;

use crate::access::{Access, PassKind, Use};
use crate::backend::{Backend as _, Rhi};
use crate::cache::{Lookup, PlanCache};
use crate::context::{ListSeq, Lists, NO_VIEW, PassContext, Recorder, RenderPass, RhiParts};
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
    /// Let independent passes run together ([`RenderGraph::set_overlap`]).
    pub overlap: bool,
    /// Where the passes' GPU times go, for the profiler ([`marlin_profile::set_gpu_timings`]).
    pub timeline: Option<&'static GpuTimeline>,
}

impl Default for FrameContextDesc {
    fn default() -> Self {
        Self {
            arena_capacity: 256 << 20,
            upload_chunk: 4 << 20,
            single_queue: false,
            overlap: false,
            timeline: None,
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
    /// Command lists submitted (each chunk and each `ctx.parallel` sub-list is one).
    pub lists: usize,
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
    // Fields drop in order: the views go before the resources they describe, so no descriptor
    // in the heap the GPU may still be reading names a destroyed resource.
    /// By access index in the graph.
    views: Vec<Option<ShaderView>>,
    rtvs: Vec<Option<RenderTargetView>>,
    dsvs: Vec<Option<DepthStencilView>>,
    textures: Vec<Option<Texture>>,
    buffers: Vec<Option<Buffer>>,
}

struct Slot {
    /// Per queue (graphics, compute, copy): command lists free to record, and those the slot's
    /// last frame submitted (reset and freed when the slot comes round again).
    free: [Mutex<Vec<Recorder>>; 3],
    used: [Vec<Recorder>; 3],
    /// Per queue: timestamp queries and where they are resolved to.
    queries: [Option<(QueryPool, Buffer)>; 3],
    /// The passes the slot's last frame timed, and the profiler frame that submitted them.
    timing: Vec<TimedPass>,
    timing_frame: u64,
    /// Fence value of the slot's last submission.
    value: u64,
    /// Buffers, textures, targets.
    heaps: [Option<Heap>; 3],
    physical: Vec<Physical>,
    upload: Mutex<Upload>,
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

/// Whether passes are timed on the GPU ([`FrameContext::gpu_timings`]): in every build but
/// shipping.
const TIMESTAMPS: bool = !cfg!(marlin_build = "shipping");

/// The smallest readback buffer; larger ones are powers of two, so recycled ones fit.
const READBACK_MIN: u64 = 256;
/// Most taken readback buffers kept for reuse.
const READBACK_POOL: usize = 64;

struct PendingReadback {
    ticket: Readback,
    buffer: Buffer,
    /// Bytes the readback holds: the buffer may be larger (it is recycled).
    size: usize,
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
    /// Readback buffers whose data was taken, by size (powers of two), for the next ones.
    free_readbacks: Vec<Buffer>,
    stats: Option<FrameStats>,
    single_queue: bool,
    overlap: bool,
    /// Recording time per pass name (hash), microseconds, smoothed: how passes are split into
    /// command lists.
    costs: HashMap<u64, u32>,
    timeline: Option<&'static GpuTimeline>,
    /// The GPU times of the last frame whose timestamps were read.
    gpu_times: Vec<PassTiming>,
    /// Timestamp ticks per second, per queue.
    frequencies: [u64; 3],
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
                free: Default::default(),
                used: Default::default(),
                queries: [None, None, None],
                timing: Vec::new(),
                timing_frame: 0,
                value: 0,
                heaps: [None, None, None],
                physical: Vec::new(),
                upload: Mutex::new(Upload {
                    chunks: Vec::new(),
                    current: 0,
                    chunk_size: desc.upload_chunk,
                }),
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
            free_readbacks: Vec::new(),
            stats: None,
            single_queue: desc.single_queue,
            overlap: desc.overlap,
            costs: HashMap::new(),
            timeline: desc.timeline,
            gpu_times: Vec::new(),
            frequencies: core::array::from_fn(|q| {
                device
                    .queue_for(sync::queue_type(q as u8))
                    .timestamp_frequency()
                    .unwrap_or(1)
            }),
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
        self.execute_on(device, None, declare)
    }

    /// As [`execute`](Self::execute), recording the command lists as jobs of `jobs` (0007 §6):
    /// each queue's passes are split into lists of about [`CHUNK_COST_US`] of recording, by
    /// the time each pass took the last time (its `b.cost` hint before that), and the lists are
    /// recorded at once and submitted in order. Without `jobs`, the same lists are recorded one
    /// after another on this thread.
    pub fn execute_on<'env, R>(
        &mut self,
        device: &Device,
        jobs: Option<&JobSystem>,
        declare: impl for<'g> FnOnce(&mut RenderGraph<'g>, &'g FrameInfo<'env>) -> R,
    ) -> Result<R> {
        let frame = self.states.frame();
        // Outlives the graph, which borrows it for `'g`.
        let info = FrameInfo {
            frame,
            _env: PhantomData,
        };
        let slot_index = (frame % FRAMES_IN_FLIGHT as u64) as usize;
        {
            marlin_profile::profile_scope!("FrameWait");
            self.fence.wait(self.slots[slot_index].value, None)?;
        }
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
            free_readbacks,
            stats,
            single_queue,
            overlap,
            costs,
            timeline,
            gpu_times,
            frequencies,
        } = self;

        // The slot's previous frame finished: its pool, upload memory and arena are free.
        let slot = &mut slots[slot_index];
        read_timings(slot, device, timeline, gpu_times, frequencies);
        for (used, free) in slot.used.iter_mut().zip(&slot.free) {
            let mut free = free.lock();
            for mut recorder in used.drain(..) {
                // SAFETY: the fence shows the GPU finished the slot's last frame, which covers
                // every list recorded from the slot's pools.
                unsafe { recorder.pool.reset()? };
                free.push(recorder);
            }
        }
        slot.upload.get_mut().reset();
        // SAFETY: frame `frame - FRAMES_IN_FLIGHT`, the arena's last user, ended inside its own
        // `execute` (its graph and closures are gone), and only this frame allocates from it.
        let arena: &Arena = unsafe { arenas.begin(frame) };

        let mut g = RenderGraph::with_states(arena, states);
        g.set_single_queue(*single_queue);
        g.set_overlap(*overlap);
        let out = {
            marlin_profile::profile_scope!("Declare");
            declare(&mut g, &info)
        };
        let (plan_index, lookup) = {
            marlin_profile::profile_scope!("Plan");
            cache.ensure(&g, device)?
        };
        let plan = cache.get(plan_index);

        // Plans the cache let go: their resources go once their slot's GPU work is done. This
        // slot's go after this frame: it places its resources in the same heap memory, and a
        // resource is not destroyed while a frame reading that memory runs (the validation
        // layer, which tracks buffers by address, would see it in use).
        let mut evicted_here: Vec<Physical> = Vec::new();
        for (s, slot) in slots.iter_mut().enumerate() {
            let mut i = 0;
            while i < slot.physical.len() {
                if cache.contains(slot.physical[i].key) {
                    i += 1;
                } else {
                    let p = slot.physical.swap_remove(i);
                    if s == slot_index {
                        evicted_here.push(p);
                    } else {
                        retired.push((slot.value, Box::new(p)));
                    }
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
        let readback_zone = marlin_profile::Zone::begin({
            static SITE: marlin_profile::ZoneSite = marlin_profile::ZoneSite {
                name: "Readbacks",
                file: file!(),
                line: line!(),
            };
            &SITE
        });
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
            // A taken buffer of the size class, or a new one: a new one each frame costs a
            // driver allocation.
            let class = size.max(READBACK_MIN).next_power_of_two();
            let buffer = match free_readbacks.iter().position(|b| b.size() == class) {
                Some(i) => free_readbacks.swap_remove(i),
                None => {
                    let buffer = device.create_buffer(&RhiBufferDesc {
                        size: class,
                        usage: BufferUsage::COPY_DST,
                        memory: MemoryLocation::Readback,
                    })?;
                    buffer.set_name("RenderGraphReadback");
                    created += 1;
                    buffer
                }
            };
            readbacks.push(PendingReadback {
                ticket: Readback { frame, index },
                buffer,
                size: size as usize,
                value: *next_value,
                rows,
            });
        }

        drop(readback_zone);
        // Record: what each retained pass needs, in a form the recording jobs can share.
        let mut execs: Vec<Option<Exec<'_, Rhi>>, &Arena> =
            Vec::with_capacity_in(g.passes.len(), arena);
        execs.extend(g.passes.iter_mut().map(|p| p.exec.take()));
        let descs: Vec<TextureDesc, &Arena> = {
            let mut v = Vec::with_capacity_in(g.textures.len(), arena);
            v.extend(g.textures.iter().map(|t| t.desc));
            v
        };
        let mut records: Vec<PassRecord<'_>, &Arena> =
            Vec::with_capacity_in(plan.passes.len(), arena);
        for pp in &plan.passes {
            let pass = &g.passes[pp.node as usize];
            let readback = match pass.builtin {
                Some(Builtin::ReadbackBuffer(index) | Builtin::ReadbackTexture(index)) => Some(
                    &readbacks[first_readback..]
                        .iter()
                        .find(|r| r.ticket.index == index)
                        .expect("made above")
                        .buffer,
                ),
                _ => None,
            };
            let render = (pass.kind == PassKind::Raster && pass.builtin.is_none())
                .then(|| render_pass_of(&g, pass, physical, import_views, &textures, &buffers));
            records.push(PassRecord {
                name: intern(pass.name),
                builtin: pass.builtin,
                accesses: pass.accesses.clone(),
                render,
                readback,
            });
        }

        // Chunks of each segment, by estimated recording cost, each a job and its own lists.
        // Pass times are for the profiler and plan dumps: not in shipping builds.
        let timed = [
            TIMESTAMPS,
            TIMESTAMPS,
            TIMESTAMPS && device.features().copy_queue_timestamps,
        ];
        let mut per_queue = [0u32; 3];
        let mut chunks: Vec<Chunk, &Arena> = Vec::new_in(arena);
        for (si, segment) in plan.segments.iter().enumerate() {
            let q = sync::queue_index(segment.queue) as usize;
            let passes = plan.segment_passes(si);
            let mut start = 0;
            let mut cost = 0;
            for (i, &pos) in passes.iter().enumerate() {
                let pass = &g.passes[plan.passes[pos as usize].node as usize];
                cost += costs
                    .get(&fast_hash(pass.name))
                    .copied()
                    .or(pass.cost_us)
                    .unwrap_or(DEFAULT_COST_US);
                let last = i + 1 == passes.len();
                if cost >= CHUNK_COST_US || last {
                    chunks.push(Chunk {
                        segment: si as u32,
                        queue: q as u8,
                        passes: &passes[start..=i],
                        first_query: per_queue[q] * 2,
                    });
                    per_queue[q] += (i + 1 - start) as u32;
                    start = i + 1;
                    cost = 0;
                }
            }
        }
        for q in 0..3 {
            let needed = u64::from(per_queue[q]) * 2;
            let have = slot.queries[q]
                .as_ref()
                .map_or(0, |(p, _)| u64::from(p.desc().count));
            if timed[q] && needed > have {
                let count = needed.next_power_of_two().max(64) as u32;
                let kind = if q == 2 {
                    QueryKind::CopyTimestamp
                } else {
                    QueryKind::Timestamp
                };
                let pool = device.create_query_pool(&QueryPoolDesc { kind, count })?;
                let results = device.create_buffer(&RhiBufferDesc {
                    size: u64::from(count) * 8,
                    usage: BufferUsage::COPY_DST,
                    memory: MemoryLocation::Readback,
                })?;
                slot.queries[q] = Some((pool, results));
            }
        }
        let retire_now: Mutex<Vec<Box<dyn Send>>> = Mutex::new(Vec::new());
        let env = RecordEnv {
            device,
            jobs,
            plan,
            records: &records,
            recs: &g.accesses,
            views: &views,
            textures: &textures,
            buffers: &buffers,
            shapes: &shapes,
            descs: &descs,
            upload: &slot.upload,
            retire: &retire_now,
        };
        let lists: [Lists<'_>; 3] = core::array::from_fn(|q| Lists {
            device,
            queue: sync::queue_type(q as u8),
            free: &slot.free[q],
        });
        let mut work: Vec<(usize, Vec<Option<Exec<'_, Rhi>>>)> = Vec::with_capacity(chunks.len());
        for (ci, chunk) in chunks.iter().enumerate() {
            let mine = chunk
                .passes
                .iter()
                .map(|&pos| execs[plan.passes[pos as usize].node as usize].take())
                .collect();
            work.push((ci, mine));
        }
        let mut outs: Vec<Option<Result<ChunkOut>>> = Vec::with_capacity(chunks.len());
        outs.resize_with(chunks.len(), || None);
        {
            marlin_profile::profile_scope!("Record");
            let (env, chunks, lists, slot_queries) = (&env, &chunks, &lists, &slot.queries);
            let run = move |ci: usize, execs: Vec<Option<Exec<'_, Rhi>>>| {
                let chunk = &chunks[ci];
                let q = chunk.queue as usize;
                let queries = slot_queries[q]
                    .as_ref()
                    .filter(|_| timed[q])
                    .map(|(p, b)| (p, b, chunk.first_query));
                record_chunk(env, &lists[q], chunk.passes, execs, queries)
            };
            match jobs {
                Some(jobs) => jobs.scope(|s| {
                    for ((ci, execs), out) in work.drain(..).zip(outs.iter_mut()) {
                        let run = &run;
                        s.spawn(move || *out = Some(run(ci, execs)));
                    }
                }),
                None => {
                    for ((ci, execs), out) in work.drain(..).zip(outs.iter_mut()) {
                        *out = Some(run(ci, execs));
                    }
                }
            }
        }
        let mut segment_lists: Vec<Vec<Recorder>> = Vec::with_capacity(plan.segments.len());
        segment_lists.resize_with(plan.segments.len(), Vec::new);
        let mut timings: Vec<TimedPass> = Vec::new();
        for (chunk, out) in chunks.iter().zip(outs.drain(..)) {
            let out = out.expect("every chunk ran")?;
            for &(pos, micros) in &out.cpu {
                let name = g.passes[plan.passes[pos as usize].node as usize].name;
                let c = costs.entry(fast_hash(name)).or_insert(micros);
                *c = (*c * 3 + micros) / 4;
            }
            if timed[chunk.queue as usize] {
                for (i, &pos) in chunk.passes.iter().enumerate() {
                    timings.push(TimedPass {
                        name: records[pos as usize].name,
                        queue: chunk.queue,
                        query: chunk.first_query + 2 * i as u32,
                    });
                }
            }
            segment_lists[chunk.segment as usize].extend(out.lists);
        }

        let list_count: usize = segment_lists.iter().map(Vec::len).sum();

        // Submit, segment by segment.
        let mut segment_values: Vec<u64, &Arena> =
            Vec::with_capacity_in(plan.segments.len(), arena);
        let mut waited_last_frame = [false; 3];
        let previous_frame_end = *next_value - 1;
        let mut last_async = [0u64; 3];
        for (si, segment) in plan.segments.iter().enumerate() {
            let q = sync::queue_index(segment.queue) as usize;
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
            let refs: SmallVec<&CommandList, 8> =
                segment_lists[si].iter().map(|r| &r.list).collect();
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
                    lists: &refs,
                    signals: &[(signal_fence, value)],
                })?
            };
            segment_values.push(value);
        }
        // The lists wait for the slot's next turn to be reset and reused.
        for (segment, recorders) in plan.segments.iter().zip(segment_lists.drain(..)) {
            slot.used[sync::queue_index(segment.queue) as usize].extend(recorders);
        }
        slot.timing = timings;
        slot.timing_frame = marlin_profile::frame_index();
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
        // Done with the frame's tables (they borrow the graph, the caches and the state table).
        drop(records);
        drop(textures);
        drop(buffers);
        drop(work);
        slot.value = value;
        // Readbacks hold their data once the whole frame is done.
        for r in &mut readbacks[first_readback..] {
            r.value = value;
        }
        for object in retire_now.into_inner() {
            retired.push((value, object));
        }
        for p in evicted_here {
            retired.push((value, Box::new(p)));
        }

        *stats = Some(FrameStats {
            key: plan.key,
            lookup,
            passes: plan.passes.len(),
            lists: list_count,
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
        let size = r.size;
        // SAFETY: the buffer is mapped for its life, at least `size` bytes long, and the GPU
        // finished writing it (the fence passed its value).
        let bytes = unsafe { core::slice::from_raw_parts(base.as_ptr(), size) };
        let out = match r.rows {
            None => bytes.to_vec(),
            Some((row_bytes, pitch, rows)) => {
                let mut out = Vec::with_capacity(row_bytes as usize * rows as usize);
                for row in 0..rows as usize {
                    let start = row * pitch as usize;
                    out.extend_from_slice(&bytes[start..start + row_bytes as usize]);
                }
                out
            }
        };
        // The GPU is done with it: the next readback of its size takes it.
        if self.free_readbacks.len() < READBACK_POOL {
            self.free_readbacks.push(r.buffer);
        }
        Some(out)
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

/// A raster pass's attachments, load and store ops, and the size of its first attachment.
fn render_pass_of<'a>(
    g: &RenderGraph<'_>,
    pass: &crate::graph::PassNode<'_, Rhi>,
    physical: &'a Physical,
    import_views: &'a HashMap<(u64, ViewSpec), Cached>,
    textures: &[Option<&'a Texture>],
    buffers: &[Option<&'a Buffer>],
) -> RenderPass<'a> {
    let attachment_view = |access: u32| -> &'a View {
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
    let first = pass
        .colors
        .first()
        .map(|c| (c.texture, c.mip))
        .or(pass.depth.map(|d| (d.texture, d.mip)));
    let size = first
        .map(|(t, mip)| {
            textures[t as usize]
                .expect("an attachment")
                .desc()
                .mip_size(mip)
        })
        .or(pass.area);
    RenderPass {
        colors,
        depth,
        size,
        parallel: pass.parallel,
        done: false,
    }
}

/// Queue `q`'s fence: graphics, compute or copy.
fn queue_fence<'f>(graphics: &'f Fence, others: &'f [Fence; 2], q: usize) -> &'f Fence {
    match q {
        0 => graphics,
        q => &others[q - 1],
    }
}

/// Recording one list takes about this long: long enough that a list is worth a job, short
/// enough to spread a frame over the workers (0007 §4.7: 0.1–0.3 ms).
pub const CHUNK_COST_US: u32 = 200;
/// The recording cost assumed for a pass never timed and without a `b.cost` hint.
const DEFAULT_COST_US: u32 = 25;

/// What recording one retained pass needs, shareable between the recording jobs.
struct PassRecord<'a> {
    name: &'static str,
    builtin: Option<Builtin>,
    accesses: core::ops::Range<u32>,
    /// Raster passes.
    render: Option<RenderPass<'a>>,
    readback: Option<&'a Buffer>,
}

/// Consecutive passes of one segment recorded by one job into lists of their own.
struct Chunk<'p> {
    segment: u32,
    queue: u8,
    /// Positions of retained passes.
    passes: &'p [u32],
    /// The chunk's first timestamp query on its queue: two per pass.
    first_query: u32,
}

struct ChunkOut {
    lists: Vec<Recorder>,
    /// Recording time per pass position, microseconds.
    cpu: Vec<(u32, u32)>,
}

/// What every recording job reads.
struct RecordEnv<'a> {
    device: &'a Device,
    jobs: Option<&'a JobSystem>,
    plan: &'a CompiledPlan,
    records: &'a [PassRecord<'a>],
    recs: &'a [AccessRec],
    views: &'a [u32],
    textures: &'a [Option<&'a Texture>],
    buffers: &'a [Option<&'a Buffer>],
    shapes: &'a [(u32, u32)],
    descs: &'a [TextureDesc],
    upload: &'a Mutex<Upload>,
    retire: &'a Mutex<Vec<Box<dyn Send>>>,
}

/// A pass whose GPU time a frame slot will read.
struct TimedPass {
    name: &'static str,
    queue: u8,
    /// Its first query; the second follows.
    query: u32,
}

/// One pass's GPU time in a frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PassTiming {
    pub name: &'static str,
    pub queue: QueueType,
    pub nanoseconds: u64,
}

/// Records one chunk: barriers, timestamps, markers and the passes' own commands.
fn record_chunk<'a>(
    env: &RecordEnv<'a>,
    lists: &Lists<'a>,
    passes: &[u32],
    execs: Vec<Option<Exec<'_, Rhi>>>,
    queries: Option<(&QueryPool, &Buffer, u32)>,
) -> Result<ChunkOut> {
    let mut seq = ListSeq {
        current: lists.take()?,
        done: Vec::new(),
    };
    let mut cpu = Vec::with_capacity(passes.len());
    for (i, (&pos, exec)) in passes.iter().zip(execs).enumerate() {
        let started = Instant::now();
        let record = &env.records[pos as usize];
        let recs = &env.recs[record.accesses.start as usize..record.accesses.end as usize];
        let list = &mut seq.current.list;
        list.begin_event(record.name);
        if let Some((pool, _, first)) = queries {
            list.write_timestamp(pool, first + 2 * i as u32);
        }
        record_barriers(list, env.plan, pos as usize, env.textures, env.buffers);
        match record.builtin {
            Some(Builtin::Present) => {}
            Some(Builtin::ReadbackBuffer(_) | Builtin::ReadbackTexture(_)) => {
                let dst = record.readback.expect("made with the frame");
                let rec = &recs[0];
                match rec.res {
                    Res::Buffer(b) => {
                        let src = env.buffers[b as usize].expect("a used buffer");
                        list.copy_buffer(src, rec.bytes.0, dst, 0, rec.bytes.1);
                    }
                    Res::Texture(t) => {
                        let src = env.textures[t as usize].expect("a used texture");
                        let copy = texture_copy(&env.descs[t as usize], rec);
                        list.copy_texture_to_buffer(src, dst, &copy);
                    }
                }
            }
            Some(Builtin::Capture) => match (recs[0].res, recs[1].res) {
                (Res::Texture(s), Res::Texture(d)) => {
                    let src = env.textures[s as usize].expect("a used texture");
                    let dst = env.textures[d as usize].expect("a used texture");
                    let desc = src.desc();
                    for layer in 0..desc.array_layers {
                        for mip in 0..desc.mip_levels {
                            let (w, h) = desc.mip_size(mip);
                            list.copy_texture(
                                src,
                                dst,
                                &TextureCopy {
                                    src_mip: mip,
                                    src_layer: layer,
                                    src_offset: [0; 3],
                                    dst_mip: mip,
                                    dst_layer: layer,
                                    dst_offset: [0; 3],
                                    size: [w, h, desc.mip_depth(mip)],
                                },
                            );
                        }
                    }
                }
                (Res::Buffer(s), Res::Buffer(d)) => {
                    let src = env.buffers[s as usize].expect("a used buffer");
                    let dst = env.buffers[d as usize].expect("a used buffer");
                    list.copy_buffer(src, 0, dst, 0, src.desc().size);
                }
                _ => unreachable!("a capture copies into its own kind"),
            },
            None => {
                let eager = record.render.as_ref().filter(|r| !r.parallel);
                if let Some(r) = eager {
                    r.begin(list, true, true);
                }
                let mut ctx = PassContext {
                    parts: RhiParts {
                        seq: &mut seq,
                        lists,
                        jobs: env.jobs,
                        device: env.device,
                        pass: record.name,
                        recs,
                        views: &env.views
                            [record.accesses.start as usize..record.accesses.end as usize],
                        textures: env.textures,
                        buffers: env.buffers,
                        shapes: env.shapes,
                        upload: env.upload,
                        retire: env.retire,
                        render: record.render.clone(),
                        nested: false,
                    },
                };
                if let Some(exec) = exec {
                    exec(&mut ctx);
                }
                let split = ctx.parts.render.as_ref().is_some_and(|r| r.done);
                drop(ctx);
                if eager.is_some() {
                    seq.current.list.end_render_pass();
                } else if let Some(r) = record.render.as_ref().filter(|_| !split) {
                    // Declared parallel but never split: the load and store ops still apply.
                    r.begin(&mut seq.current.list, true, true);
                    seq.current.list.end_render_pass();
                }
            }
        }
        let list = &mut seq.current.list;
        if let Some((pool, _, first)) = queries {
            list.write_timestamp(pool, first + 2 * i as u32 + 1);
        }
        list.end_event();
        cpu.push((pos, started.elapsed().as_micros() as u32));
    }
    if let Some((pool, results, first)) = queries {
        let count = 2 * passes.len() as u32;
        seq.current
            .list
            .resolve_queries(pool, first, count, results, u64::from(first) * 8);
    }
    seq.current.list.end()?;
    let mut lists = seq.done;
    lists.push(seq.current);
    Ok(ChunkOut { lists, cpu })
}

/// Reads the GPU times of the frame that last used the slot (its fence has passed): into the
/// last-frame list and, converted to the CPU clock, into the timeline.
fn read_timings(
    slot: &mut Slot,
    device: &Device,
    timeline: &Option<&'static GpuTimeline>,
    gpu_times: &mut Vec<PassTiming>,
    frequencies: &[u64; 3],
) {
    if slot.timing.is_empty() {
        return;
    }
    let calibrations: [Option<marlin_rhi::ClockCalibration>; 3] = core::array::from_fn(|q| {
        timeline.and_then(|_| {
            device
                .queue_for(sync::queue_type(q as u8))
                .clock_calibration()
                .ok()
        })
    });
    let cpu_per_second = i128::from(marlin_std::time::ticks_per_second());
    gpu_times.clear();
    for t in slot.timing.drain(..) {
        let q = t.queue as usize;
        let Some((_, results)) = &slot.queries[q] else {
            continue;
        };
        let base = results.mapped().expect("readback memory is mapped");
        let read = |i: u32| {
            let mut word = [0u8; 8];
            // SAFETY: the buffer is mapped for its life, holds a `u64` per query, and the GPU
            // finished writing it (the slot's fence passed).
            unsafe {
                ptr::copy_nonoverlapping(base.as_ptr().add(i as usize * 8), word.as_mut_ptr(), 8);
            }
            u64::from_le_bytes(word)
        };
        let (start, end) = (read(t.query), read(t.query + 1));
        let freq = i128::from(frequencies[q].max(1));
        let ticks = i128::from(end.saturating_sub(start));
        gpu_times.push(PassTiming {
            name: t.name,
            queue: sync::queue_type(t.queue),
            nanoseconds: (ticks * 1_000_000_000 / freq) as u64,
        });
        if let (Some(timeline), Some(cal)) = (timeline, calibrations[q]) {
            let to_cpu = |gpu: u64| {
                let delta = i128::from(gpu) - i128::from(cal.gpu);
                (i128::from(cal.cpu) + delta * cpu_per_second / freq).max(0) as u64
            };
            timeline.zones.lock().push(marlin_profile::GpuZone {
                frame: slot.timing_frame,
                name: t.name,
                queue: q as u32,
                start: to_cpu(start),
                end: to_cpu(end),
            });
        }
    }
}

/// GPU timings for the profiler: install it with [`marlin_profile::set_gpu_timings`] and give it
/// to the frame context ([`FrameContextDesc::timeline`]).
pub struct GpuTimeline {
    zones: Mutex<Vec<marlin_profile::GpuZone>>,
}

impl GpuTimeline {
    pub const fn new() -> Self {
        Self {
            zones: Mutex::new(Vec::new()),
        }
    }
}

impl Default for GpuTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for GpuTimeline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("GpuTimeline")
    }
}

impl marlin_profile::GpuTimings for GpuTimeline {
    fn collect(&self, out: &mut dyn FnMut(marlin_profile::GpuZone)) {
        for zone in self.zones.lock().drain(..) {
            out(zone);
        }
    }

    fn queue_name(&self, queue: u32) -> &str {
        match queue {
            1 => "GPU compute",
            2 => "GPU copy",
            _ => "GPU graphics",
        }
    }
}

/// Pass names live in frame arenas; timings outlive them. Interned once per distinct name.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock();
    if let Some(&n) = names.iter().find(|&&n| n == name) {
        return n;
    }
    let leaked: &'static str = Box::leak(alloc::string::String::from(name).into_boxed_str());
    names.push(leaked);
    leaked
}

impl FrameContext {
    /// The GPU time of each pass of the last frame whose timestamps came back (a couple of
    /// frames ago); none in shipping builds, which do not time passes.
    pub fn gpu_timings(&self) -> &[PassTiming] {
        &self.gpu_times
    }

    /// The copy a capture named `name` made ([`RenderGraph::capture`]), as of the last frame
    /// that captured it. Valid until the next frame.
    pub fn captured_texture(&self, name: &str) -> Option<&Texture> {
        let key = crate::retained::retained_key(name, crate::retained::CAPTURE_SCOPE);
        match self.retained.get(&(key, 0))? {
            RetainedObject::Texture(t) => Some(t),
            RetainedObject::Buffer(_) => None,
        }
    }

    pub fn captured_buffer(&self, name: &str) -> Option<&Buffer> {
        let key = crate::retained::retained_key(name, crate::retained::CAPTURE_SCOPE);
        match self.retained.get(&(key, 0))? {
            RetainedObject::Buffer(b) => Some(b),
            RetainedObject::Texture(_) => None,
        }
    }
}

impl FrameContext {
    /// The last frame's plan as JSON ([`CompiledPlan::to_json`]) with the latest GPU times.
    pub fn dump(&self) -> Option<alloc::string::String> {
        let stats = self.stats?;
        let plan = self.cache.plan(stats.key)?;
        Some(plan.to_json(&self.gpu_times))
    }
}
