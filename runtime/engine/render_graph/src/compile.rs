//! Compiling a declared graph into a [`CompiledPlan`] (0007 §4): culling, queues, dependencies,
//! usage, lifetimes, aliasing, barriers and segments.

use alloc::string::ToString;
use alloc::vec::Vec;

use marlin_containers::SmallVec;
use marlin_rhi::{
    Access as RhiAccess, BufferState, BufferUsage, HeapKind, Layout, Result, Stage,
    SubresourceRange, TextureState, TextureUsage,
};

use crate::access::{
    Effect, buffer_covers, is_read_only, texture_covers, union_buffer, union_texture,
};
use crate::alias::{self, Item};
use crate::backend::Backend;
use crate::graph::{AccessRec, RenderGraph, Res, Source};
use crate::plan::{
    BufferTransition, CompiledPlan, Lifetime, Placement, PlannedBuffer, PlannedPass,
    PlannedTexture, TextureFinal, TextureTransition,
};
use crate::sync::{self, GRAPHICS};

pub(crate) fn compile<B: Backend>(
    g: &RenderGraph<'_, B>,
    device: &B::Device,
    key: u64,
) -> Result<CompiledPlan> {
    let subs = SubIndex::new(g);
    let needed = cull(g, &subs);
    let declared: Vec<u32> = (0..g.passes.len() as u32)
        .filter(|&p| needed[p as usize])
        .collect();
    let culled = (0..g.passes.len() as u32)
        .filter(|&p| !needed[p as usize])
        .collect();

    let available = if g.single_queue {
        crate::backend::Queues {
            compute: false,
            copy: false,
        }
    } else {
        B::queues(device)
    };
    let queue_of = |p: u32| sync::queue_index(sync::resolve(g.passes[p as usize].queue, available));
    // Passes that need no order between them run together (§4.8).
    let (retained, groups) = if !g.overlap {
        let groups = (0..declared.len() as u32).map(|p| p..p + 1).collect();
        (declared, groups)
    } else {
        let queues: Vec<u8> = declared.iter().map(|&p| queue_of(p)).collect();
        let edges = sync::dependencies(g, &declared, &subs.base, subs.total);
        schedule(&declared, &queues, &edges)
    };
    let queues: Vec<u8> = retained.iter().map(|&p| queue_of(p)).collect();
    let edges = sync::dependencies(g, &retained, &subs.base, subs.total);
    let mut windows = sync::windows(&queues, &edges);
    // A group's passes may run at once: what they use is alive over the whole group.
    for (pos, w) in windows.iter_mut().enumerate() {
        let group = &groups[pos];
        *w = (w.0.min(group.start), w.1.max(group.end - 1));
    }

    let (mut textures, mut buffers) = usage_and_lifetimes(g, &retained, &windows);
    let heap_sizes = place(g, device, &mut textures, &mut buffers)?;

    let mut states = States::new(g, &subs, &textures, &buffers);
    for (pos, &node) in retained.iter().enumerate() {
        let pass = &g.passes[node as usize];
        for rec in &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize] {
            states.access(pos as u32, queues[pos], rec);
        }
    }
    let (segments, segment_passes) = sync::segments(&queues, &edges);
    // A group's barriers all go before its first pass in the segment, so they do not stand
    // between the passes.
    let mut first_in_segment = alloc::vec![0u32; retained.len()];
    for s in &segments {
        let passes = &segment_passes[s.passes.start as usize..s.passes.end as usize];
        for &p in passes {
            first_in_segment[p as usize] = passes[0];
        }
    }
    let hoist: Vec<u32> = (0..retained.len())
        .map(|pos| groups[pos].start.max(first_in_segment[pos]))
        .collect();
    states.hoist(&hoist);
    let (texture_barriers, buffer_barriers, ranges) = states.coalesce(retained.len());
    let passes = retained
        .iter()
        .zip(ranges)
        .zip(&queues)
        .map(|((&node, (tex, buf)), &q)| {
            let pass = &g.passes[node as usize];
            PlannedPass {
                node,
                name: pass.name.to_string(),
                kind: pass.kind,
                queue: sync::queue_type(q),
                texture_barriers: tex,
                buffer_barriers: buf,
            }
        })
        .collect();
    let (texture_finals, buffer_finals) = states.finals();
    Ok(CompiledPlan {
        key,
        passes,
        culled,
        edges,
        textures,
        buffers,
        texture_barriers,
        buffer_barriers,
        texture_finals,
        buffer_finals,
        heap_sizes,
        texture_names: g.textures.iter().map(|t| t.name.to_string()).collect(),
        buffer_names: g.buffers.iter().map(|b| b.name.to_string()).collect(),
        segments,
        segment_passes,
    })
}

/// The order the retained passes (`declared`, in declaration order, on `queues`, with the
/// dependencies `edges`) run in (0007 §4.8): by level, each pass as late as what depends on it
/// allows (a pass's level is the longest chain of passes depending on it, counted from the end),
/// then by queue, then in declaration order. The passes of one level and queue are a group: none
/// depends on another, so they may run at once. Returns the passes in that order and, per position, its group's positions.
fn schedule(
    declared: &[u32],
    queues: &[u8],
    edges: &[(u32, u32)],
) -> (Vec<u32>, Vec<core::ops::Range<u32>>) {
    let n = declared.len();
    let mut succs: Vec<SmallVec<u32, 4>> = (0..n).map(|_| SmallVec::new()).collect();
    for &(a, b) in edges {
        succs[a as usize].push(b);
    }
    // Edges go from earlier to later declarations.
    let mut height = alloc::vec![0u32; n];
    for p in (0..n).rev() {
        height[p] = succs[p]
            .iter()
            .map(|&s| height[s as usize] + 1)
            .max()
            .unwrap_or(0);
    }
    let top = height.iter().copied().max().unwrap_or(0);
    let key = |p: u32| (top - height[p as usize], queues[p as usize]);
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_by_key(|&p| (key(p), p));
    let mut groups = alloc::vec![0..0; n];
    let mut start = 0;
    for i in 1..=n {
        if i == n || key(order[i]) != key(order[start]) {
            groups[start..i].fill(start as u32..i as u32);
            start = i;
        }
    }
    (
        order.iter().map(|&p| declared[p as usize]).collect(),
        groups,
    )
}

/// Flat indices of every texture's subresources: layer-major, `base[t] + layer * mips + mip`.
struct SubIndex {
    base: Vec<u32>,
    total: u32,
}

impl SubIndex {
    fn new<B: Backend>(g: &RenderGraph<'_, B>) -> Self {
        let mut base = Vec::with_capacity(g.textures.len());
        let mut total = 0;
        for t in &g.textures {
            base.push(total);
            total += t.desc.subresources();
        }
        Self { base, total }
    }

    /// Flat indices of the subresources an access covers.
    fn of<'a, B: Backend>(
        &'a self,
        g: &'a RenderGraph<'_, B>,
        texture: u32,
        rec: &'a AccessRec,
    ) -> impl Iterator<Item = u32> + 'a {
        let mips = g.textures[texture as usize].desc.mip_levels;
        let base = self.base[texture as usize];
        let r = rec.range;
        (r.base_layer..r.base_layer + r.layer_count).flat_map(move |layer| {
            (r.base_mip..r.base_mip + r.mip_count).map(move |mip| base + layer * mips + mip)
        })
    }
}

/// Which passes affect the frame's observable result: side-effect passes, and passes writing
/// contents a kept pass reads or an imported resource keeps. Walks backwards keeping a live flag
/// per subresource ("someone later reads what is here now").
fn cull<B: Backend>(g: &RenderGraph<'_, B>, subs: &SubIndex) -> Vec<bool> {
    let mut live_tex = alloc::vec![false; subs.total as usize];
    let mut live_buf = alloc::vec![false; g.buffers.len()];
    for (t, node) in g.textures.iter().enumerate() {
        if node.import.as_ref().is_some_and(|i| !i.read_only) {
            let base = subs.base[t] as usize;
            live_tex[base..base + node.desc.subresources() as usize].fill(true);
        }
    }
    for (b, node) in g.buffers.iter().enumerate() {
        live_buf[b] = node.import.as_ref().is_some_and(|i| !i.read_only);
    }
    let mut needed = alloc::vec![false; g.passes.len()];
    for p in (0..g.passes.len()).rev() {
        let pass = &g.passes[p];
        let recs = &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize];
        let need = pass.side_effect
            || recs.iter().any(|rec| {
                rec.effect.writes()
                    && match rec.res {
                        Res::Texture(t) => subs.of(g, t, rec).any(|s| live_tex[s as usize]),
                        Res::Buffer(b) => live_buf[b as usize],
                    }
            });
        if !need {
            continue;
        }
        needed[p] = true;
        for rec in recs.iter().filter(|r| r.effect == Effect::Overwrite) {
            if let Res::Texture(t) = rec.res {
                subs.of(g, t, rec)
                    .for_each(|s| live_tex[s as usize] = false);
            }
        }
        for rec in recs.iter().filter(|r| r.effect.reads()) {
            match rec.res {
                Res::Texture(t) => subs.of(g, t, rec).for_each(|s| live_tex[s as usize] = true),
                Res::Buffer(b) => live_buf[b as usize] = true,
            }
        }
    }
    needed
}

/// Usage, and lifetimes as the union of the windows of the passes using each resource.
fn usage_and_lifetimes<B: Backend>(
    g: &RenderGraph<'_, B>,
    retained: &[u32],
    windows: &[(u32, u32)],
) -> (Vec<PlannedTexture>, Vec<PlannedBuffer>) {
    let mut textures: Vec<PlannedTexture> = g
        .textures
        .iter()
        .map(|_| PlannedTexture {
            usage: TextureUsage::default(),
            lifetime: None,
            placement: None,
        })
        .collect();
    let mut buffers: Vec<PlannedBuffer> = g
        .buffers
        .iter()
        .map(|_| PlannedBuffer {
            usage: BufferUsage::default(),
            lifetime: None,
            placement: None,
        })
        .collect();
    let extend = |life: &mut Option<Lifetime>, (first, last): (u32, u32)| {
        *life = Some(match *life {
            None => Lifetime { first, last },
            Some(l) => Lifetime {
                first: l.first.min(first),
                last: l.last.max(last),
            },
        });
    };
    for (pos, &node) in retained.iter().enumerate() {
        let pass = &g.passes[node as usize];
        for rec in &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize] {
            match rec.res {
                Res::Texture(t) => {
                    let t = &mut textures[t as usize];
                    t.usage |= rec.usage.texture_usage();
                    extend(&mut t.lifetime, windows[pos]);
                }
                Res::Buffer(b) => {
                    let b = &mut buffers[b as usize];
                    b.usage |= rec.usage.buffer_usage();
                    extend(&mut b.lifetime, windows[pos]);
                }
            }
        }
    }
    (textures, buffers)
}

/// Places the used transients in the transient heaps.
fn place<B: Backend>(
    g: &RenderGraph<'_, B>,
    device: &B::Device,
    textures: &mut [PlannedTexture],
    buffers: &mut [PlannedBuffer],
) -> Result<[u64; 3]> {
    let mut items = Vec::new();
    let mut owners = Vec::new();
    for (i, t) in textures.iter().enumerate() {
        let (Some(lifetime), None) = (t.lifetime, &g.textures[i].import) else {
            continue;
        };
        let desc = g.textures[i].desc.to_rhi(t.usage);
        let heap = if t
            .usage
            .intersects(TextureUsage::RENDER_TARGET | TextureUsage::DEPTH_STENCIL)
        {
            HeapKind::Targets
        } else {
            HeapKind::Textures
        };
        items.push(Item {
            lifetime,
            req: B::texture_requirements(device, &desc)?,
            heap,
        });
        owners.push(Res::Texture(i as u32));
    }
    for (i, b) in buffers.iter().enumerate() {
        let (Some(lifetime), None) = (b.lifetime, &g.buffers[i].import) else {
            continue;
        };
        let desc = g.buffers[i].desc.to_rhi(b.usage);
        items.push(Item {
            lifetime,
            req: B::buffer_requirements(device, &desc),
            heap: HeapKind::Buffers,
        });
        owners.push(Res::Buffer(i as u32));
    }
    let (placements, sizes) = alias::place(&items);
    for (owner, p) in owners.into_iter().zip(placements) {
        match owner {
            Res::Texture(t) => textures[t as usize].placement = Some(p),
            Res::Buffer(b) => buffers[b as usize].placement = Some(p),
        }
    }
    Ok(sizes)
}

/// What barrier tracking needs of a texture or buffer state. Buffers have no layout: theirs is
/// always [`Layout::Common`].
trait SyncState: Copy + PartialEq {
    const HAS_LAYOUT: bool;
    fn stage(self) -> Stage;
    fn access(self) -> RhiAccess;
    fn layout(self) -> Layout;
    fn make(stage: Stage, access: RhiAccess, layout: Layout) -> Self;
    fn covers(self, want: Self) -> bool;
    fn union(self, other: Self) -> Self;
}

impl SyncState for TextureState {
    const HAS_LAYOUT: bool = true;
    fn stage(self) -> Stage {
        self.stage
    }
    fn access(self) -> RhiAccess {
        self.access
    }
    fn layout(self) -> Layout {
        self.layout
    }
    fn make(stage: Stage, access: RhiAccess, layout: Layout) -> Self {
        Self {
            stage,
            access,
            layout,
        }
    }
    fn covers(self, want: Self) -> bool {
        texture_covers(self, want)
    }
    fn union(self, other: Self) -> Self {
        union_texture(self, other)
    }
}

impl SyncState for BufferState {
    const HAS_LAYOUT: bool = false;
    fn stage(self) -> Stage {
        self.stage
    }
    fn access(self) -> RhiAccess {
        self.access
    }
    fn layout(self) -> Layout {
        Layout::Common
    }
    fn make(stage: Stage, access: RhiAccess, _: Layout) -> Self {
        Self { stage, access }
    }
    fn covers(self, want: Self) -> bool {
        buffer_covers(self, want)
    }
    fn union(self, other: Self) -> Self {
        union_buffer(self, other)
    }
}

/// One subresource's (or buffer's) synchronization state while walking the passes.
///
/// Accesses on one queue are ordered by barriers on that queue; accesses on different queues by
/// the waits between segments (every dependency between passes on different queues is one,
/// [`sync::dependencies`]). A semaphore wait orders and makes visible everything before its
/// signal for all later work, so a barrier on the consumer's queue only has to change the layout:
/// its source is [`Stage::ALL`] (chaining with the wait) and no access.
struct Track<S> {
    /// The last access's state (merged, for reads): what the frame ends in.
    state: S,
    layout: Layout,
    /// The last write: its queue and state. Imports that start in a writing state count as
    /// written on the graphics queue.
    writer: Option<(u8, S)>,
    /// Per queue, the merged state of the reads since the last write or layout change, and the
    /// barrier on that queue that made the write visible to them (which later readers widen).
    reads: [Option<(S, Option<u32>)>; 3],
    touched: bool,
    /// The last write was unordered: the next unordered write in its state needs no barrier.
    unordered: bool,
}

impl<S: SyncState> Track<S> {
    fn new(initial: S) -> Self {
        let mut track = Self {
            state: initial,
            layout: initial.layout(),
            writer: None,
            reads: [None; 3],
            touched: false,
            unordered: false,
        };
        let defined = initial.layout() != Layout::Undefined || !S::HAS_LAYOUT;
        let nothing = initial.stage().is_empty() && initial.access().is_empty();
        if defined && is_read_only(initial.access()) && !(nothing && !S::HAS_LAYOUT) {
            track.reads[GRAPHICS as usize] = Some((initial, None));
        } else if defined && !is_read_only(initial.access()) {
            track.writer = Some((GRAPHICS, initial));
        }
        track
    }

    /// What an access on `q` that conflicts with the write (and, with `readers`, the reads since)
    /// must wait for: the stages and accesses on `q`, and whether any were on another queue.
    fn pending(&self, q: u8, readers: bool) -> (Stage, RhiAccess, bool) {
        let (mut stage, mut access, mut cross) = (Stage::NONE, RhiAccess::NONE, false);
        // Reads on `q` since the write came after a barrier that already ordered it.
        let read_here = readers && self.reads[q as usize].is_some();
        match self.writer {
            Some((wq, ws)) if wq == q => {
                if !read_here {
                    stage |= ws.stage();
                    access |= ws.access();
                }
            }
            Some(_) => cross = true,
            None => {}
        }
        if readers {
            for (rq, r) in self.reads.iter().enumerate() {
                if let Some((s, _)) = r {
                    if rq as u8 == q {
                        stage |= s.stage();
                        access |= s.access();
                    } else {
                        cross = true;
                    }
                }
            }
        }
        (stage, access, cross)
    }
}

struct Raw<S> {
    pass: u32,
    /// The texture or buffer slot.
    res: u32,
    /// The flat subresource (textures).
    sub: u32,
    before: S,
    after: S,
    undefined_from: Layout,
    /// A transient's first use: `undefined_from` is filled in from its final layout.
    first_use: bool,
}

/// What one access needs of the barrier tracking.
enum Step<S> {
    Nothing,
    Barrier {
        before: S,
        undefined_from: Layout,
    },
    /// Widen the earlier barrier `index` to `want` as well.
    Widen(u32),
}

/// One access on queue `q` of a tracked subresource; `fresh` is the stage a transient's first
/// use waits for (the last uses of the memory's previous holders); `unordered` for an unordered
/// write. Updates the track.
#[allow(clippy::too_many_arguments, reason = "one access")]
fn step<S: SyncState>(
    track: &mut Track<S>,
    q: u8,
    want: S,
    effect: Effect,
    unordered: bool,
    fresh: Option<Stage>,
    next_raw: u32,
) -> Step<S> {
    let discarded = |stage: Stage| S::make(stage, RhiAccess::NONE, Layout::Undefined);
    let undefined_from = match track.layout {
        Layout::Undefined => Layout::Common,
        l => l,
    };
    let out;
    if let Some(stage) = fresh {
        out = if !S::HAS_LAYOUT && stage.is_empty() {
            Step::Nothing
        } else {
            Step::Barrier {
                before: discarded(stage),
                undefined_from,
            }
        };
        track.writer = Some((q, want));
        track.reads = [None; 3];
        if !effect.writes() {
            track.writer = None;
            track.reads[q as usize] = Some((
                want,
                matches!(out, Step::Barrier { .. }).then_some(next_raw),
            ));
        }
    } else if unordered
        && effect.writes()
        && track.unordered
        && track.writer == Some((q, want))
        && track.reads.iter().all(Option::is_none)
        && (!S::HAS_LAYOUT || want.layout() == track.layout)
    {
        // Joins the run of unordered writes before it.
        out = Step::Nothing;
    } else if effect.writes()
        || (S::HAS_LAYOUT && (want.layout() != track.layout || track.layout == Layout::Undefined))
    {
        let (same_stage, access, cross) = track.pending(q, true);
        let undefined = S::HAS_LAYOUT && track.layout == Layout::Undefined;
        let relayout = S::HAS_LAYOUT && (undefined || want.layout() != track.layout);
        let discard = effect == Effect::Overwrite || undefined;
        let stage = if cross {
            same_stage | Stage::ALL
        } else {
            same_stage
        };
        let before = if discard {
            discarded(stage)
        } else {
            S::make(stage, access, track.layout)
        };
        // Conflicts only on other queues are ordered by the waits: a barrier is only needed to
        // change the layout.
        let same_queue = !same_stage.is_empty() || !access.is_empty();
        out = if same_queue || relayout {
            Step::Barrier {
                before,
                undefined_from,
            }
        } else {
            Step::Nothing
        };
        track.reads = [None; 3];
        if effect.writes() {
            track.writer = Some((q, want));
        } else {
            track.reads[q as usize] = Some((
                want,
                matches!(out, Step::Barrier { .. }).then_some(next_raw),
            ));
        }
    } else {
        // A read in the current layout.
        out = match track.reads[q as usize] {
            Some((s, b)) => {
                if s.covers(want) {
                    Step::Nothing
                } else {
                    let merged = s.union(want);
                    match b {
                        Some(b) => {
                            track.reads[q as usize] = Some((merged, Some(b)));
                            Step::Widen(b)
                        }
                        None => {
                            track.reads[q as usize] = Some((merged, Some(next_raw)));
                            Step::Barrier {
                                before: s,
                                undefined_from,
                            }
                        }
                    }
                }
            }
            None => match track.writer {
                Some((wq, ws)) if wq == q => {
                    track.reads[q as usize] = Some((want, Some(next_raw)));
                    Step::Barrier {
                        before: ws,
                        undefined_from,
                    }
                }
                // Written on another queue (the wait made it visible) or not at all.
                _ => {
                    track.reads[q as usize] = Some((want, None));
                    Step::Nothing
                }
            },
        };
    }
    track.state = match (effect.writes(), fresh) {
        (false, _) => track.reads[q as usize].map_or(want, |(s, _)| s),
        _ => want,
    };
    if effect.writes() {
        track.unordered = unordered;
    }
    track.layout = track.state.layout();
    track.touched = true;
    out
}

/// Walks the retained passes in order, tracking each subresource's state.
/// Per pass, its ranges of texture and of buffer transitions.
type PassBarriers = Vec<(core::ops::Range<u32>, core::ops::Range<u32>)>;

struct States<'a, 'g, B: Backend> {
    g: &'a RenderGraph<'g, B>,
    subs: &'a SubIndex,
    textures: &'a [PlannedTexture],
    buffers: &'a [PlannedBuffer],
    tex: Vec<Track<TextureState>>,
    buf: Vec<Track<BufferState>>,
    raw_tex: Vec<Raw<TextureState>>,
    raw_buf: Vec<Raw<BufferState>>,
}

impl<'a, 'g, B: Backend> States<'a, 'g, B> {
    fn new(
        g: &'a RenderGraph<'g, B>,
        subs: &'a SubIndex,
        textures: &'a [PlannedTexture],
        buffers: &'a [PlannedBuffer],
    ) -> Self {
        let mut tex = Vec::with_capacity(subs.total as usize);
        for (t, node) in g.textures.iter().enumerate() {
            let initial: &[TextureState] = match &node.import {
                None => &[TextureState::UNDEFINED],
                // A frame needing more usage than a graph-owned object has gets a new one.
                Some(i)
                    if matches!(i.object, Source::Retained(_))
                        && !i.usage.contains(textures[t].usage) =>
                {
                    &[TextureState::UNDEFINED]
                }
                Some(i) => i.initial,
            };
            for sub in 0..node.desc.subresources() as usize {
                let state = if initial.len() == 1 {
                    initial[0]
                } else {
                    initial[sub]
                };
                tex.push(Track::new(state));
            }
        }
        let buf = g
            .buffers
            .iter()
            .enumerate()
            .map(|(b, node)| {
                let initial = match &node.import {
                    None => BufferState::NONE,
                    Some(i)
                        if matches!(i.object, Source::Retained(_))
                            && !i.usage.contains(buffers[b].usage) =>
                    {
                        BufferState::NONE
                    }
                    Some(i) => i.initial,
                };
                Track::new(initial)
            })
            .collect();
        Self {
            g,
            subs,
            textures,
            buffers,
            tex,
            buf,
            raw_tex: Vec::new(),
            raw_buf: Vec::new(),
        }
    }

    fn access(&mut self, pos: u32, q: u8, rec: &AccessRec) {
        match rec.res {
            Res::Texture(t) => {
                let want = rec.usage.texture_state().expect("validated when declared");
                let transient = self.g.textures[t as usize].import.is_none();
                let mut pred_stage = None;
                let subs: SmallVec<u32, 16> = self.subs.of(self.g, t, rec).collect();
                for sub in subs {
                    let fresh = transient && !self.tex[sub as usize].touched;
                    if fresh && pred_stage.is_none() {
                        pred_stage = Some(self.alias_stage(Res::Texture(t), q));
                    }
                    let next = self.raw_tex.len() as u32;
                    let s = step(
                        &mut self.tex[sub as usize],
                        q,
                        want,
                        rec.effect,
                        rec.unordered,
                        if fresh { pred_stage } else { None },
                        next,
                    );
                    let after = self.tex[sub as usize].state;
                    record(&mut self.raw_tex, s, pos, t, sub, want, after, fresh);
                }
            }
            Res::Buffer(b) => {
                let want = rec.usage.buffer_state().expect("validated when declared");
                let fresh =
                    self.g.buffers[b as usize].import.is_none() && !self.buf[b as usize].touched;
                let pred = fresh.then(|| self.alias_stage(Res::Buffer(b), q));
                let next = self.raw_buf.len() as u32;
                let s = step(
                    &mut self.buf[b as usize],
                    q,
                    want,
                    rec.effect,
                    rec.unordered,
                    pred,
                    next,
                );
                let after = self.buf[b as usize].state;
                record(&mut self.raw_buf, s, pos, b, b, want, after, fresh);
            }
        }
    }

    /// The stages a transient's first use on `q` waits for: the last uses of transients that
    /// held its memory before it, on `q`; [`Stage::ALL`] if some were on another queue (the
    /// waits between the queues order them, and this chains with those waits).
    fn alias_stage(&self, res: Res, q: u8) -> Stage {
        let (mine, lifetime) = match res {
            Res::Texture(t) => {
                let t = &self.textures[t as usize];
                (t.placement, t.lifetime)
            }
            Res::Buffer(b) => {
                let b = &self.buffers[b as usize];
                (b.placement, b.lifetime)
            }
        };
        let (Some(mine), Some(lifetime)) = (mine, lifetime) else {
            return Stage::NONE;
        };
        let before = |p: &Option<Placement>, l: &Option<Lifetime>| matches!((p, l), (Some(p), Some(l)) if p.overlaps(&mine) && l.last < lifetime.first);
        let mut stage = Stage::NONE;
        let mut add = |(s, _, cross): (Stage, RhiAccess, bool)| {
            stage |= s;
            if cross {
                stage |= Stage::ALL;
            }
        };
        match res {
            Res::Texture(_) => {
                for (t, planned) in self.textures.iter().enumerate() {
                    if before(&planned.placement, &planned.lifetime) {
                        let base = self.subs.base[t] as usize;
                        let n = self.g.textures[t].desc.subresources() as usize;
                        for track in &self.tex[base..base + n] {
                            add(track.pending(q, true));
                        }
                    }
                }
            }
            Res::Buffer(_) => {
                for (b, planned) in self.buffers.iter().enumerate() {
                    if before(&planned.placement, &planned.lifetime) {
                        add(self.buf[b].pending(q, true));
                    }
                }
            }
        }
        stage
    }

    /// Merges per-subresource barriers into ranges, per pass. Returns the barriers and each
    /// pass's texture and buffer ranges.
    #[allow(clippy::type_complexity)]
    /// Moves each pass's barriers to the pass `to[pass]` (an earlier one or itself, never
    /// decreasing with the pass), so a group's barriers come before all of it.
    fn hoist(&mut self, to: &[u32]) {
        for raw in &mut self.raw_tex {
            raw.pass = to[raw.pass as usize];
        }
        for raw in &mut self.raw_buf {
            raw.pass = to[raw.pass as usize];
        }
    }

    fn coalesce(
        &mut self,
        passes: usize,
    ) -> (Vec<TextureTransition>, Vec<BufferTransition>, PassBarriers) {
        for raw in &mut self.raw_tex {
            if raw.first_use {
                raw.undefined_from = match self.tex[raw.sub as usize].state.layout {
                    Layout::Undefined => Layout::Common,
                    layout => layout,
                };
            }
        }
        let mut textures = Vec::new();
        let mut buffers = Vec::new();
        let mut ranges = Vec::with_capacity(passes);
        let (mut t, mut b) = (0, 0);
        for pass in 0..passes as u32 {
            let tex_start = textures.len() as u32;
            let t_end = t + self.raw_tex[t..]
                .iter()
                .take_while(|r| r.pass == pass)
                .count();
            let slice = &self.raw_tex[t..t_end];
            let mut done = alloc::vec![false; slice.len()];
            for i in 0..slice.len() {
                if done[i] {
                    continue;
                }
                let head = &slice[i];
                let mut members: SmallVec<u32, 16> = SmallVec::new();
                for (j, other) in slice.iter().enumerate().skip(i) {
                    if !done[j]
                        && other.res == head.res
                        && other.before == head.before
                        && other.after == head.after
                        && other.undefined_from == head.undefined_from
                    {
                        done[j] = true;
                        members.push(other.sub - self.subs.base[head.res as usize]);
                    }
                }
                let mips = self.g.textures[head.res as usize].desc.mip_levels;
                for range in ranges_of(&mut members, mips) {
                    textures.push(TextureTransition {
                        texture: head.res,
                        range,
                        before: head.before,
                        after: head.after,
                        undefined_from: head.undefined_from,
                    });
                }
            }
            t = t_end;
            let buf_start = buffers.len() as u32;
            while b < self.raw_buf.len() && self.raw_buf[b].pass == pass {
                let raw = &self.raw_buf[b];
                buffers.push(BufferTransition {
                    buffer: raw.res,
                    before: raw.before,
                    after: raw.after,
                });
                b += 1;
            }
            ranges.push((
                tex_start..textures.len() as u32,
                buf_start..buffers.len() as u32,
            ));
        }
        (textures, buffers, ranges)
    }

    fn finals(&self) -> (Vec<TextureFinal>, Vec<(u32, BufferState)>) {
        let mut textures = Vec::new();
        for (t, node) in self.g.textures.iter().enumerate() {
            if node.import.is_none() {
                continue;
            }
            let base = self.subs.base[t];
            let n = node.desc.subresources();
            let mut done = alloc::vec![false; n as usize];
            for i in 0..n {
                let track = &self.tex[(base + i) as usize];
                if done[i as usize] || !track.touched {
                    continue;
                }
                let mut members: SmallVec<u32, 16> = SmallVec::new();
                for j in i..n {
                    let other = &self.tex[(base + j) as usize];
                    if !done[j as usize] && other.touched && other.state == track.state {
                        done[j as usize] = true;
                        members.push(j);
                    }
                }
                for range in ranges_of(&mut members, node.desc.mip_levels) {
                    textures.push(TextureFinal {
                        texture: t as u32,
                        range,
                        state: track.state,
                    });
                }
            }
        }
        let buffers = self
            .g
            .buffers
            .iter()
            .enumerate()
            .filter(|(b, node)| node.import.is_some() && self.buf[*b].touched)
            .map(|(b, _)| (b as u32, self.buf[b].state))
            .collect();
        (textures, buffers)
    }
}

/// Applies a step's barrier work to the raw barrier list.
#[allow(clippy::too_many_arguments)]
fn record<S: SyncState>(
    raws: &mut Vec<Raw<S>>,
    step: Step<S>,
    pass: u32,
    res: u32,
    sub: u32,
    want: S,
    after: S,
    first_use: bool,
) {
    match step {
        Step::Nothing => {}
        Step::Widen(i) => {
            let raw = &mut raws[i as usize];
            raw.after = raw.after.union(want);
        }
        Step::Barrier {
            before,
            undefined_from,
        } => raws.push(Raw {
            pass,
            res,
            sub,
            before,
            after,
            undefined_from,
            first_use,
        }),
    }
}

/// Covers a set of subresources (layer-major indices within one texture of `mips` mips) with
/// rectangles: runs of mips per layer, merged across consecutive layers with the same run.
fn ranges_of(subs: &mut [u32], mips: u32) -> SmallVec<SubresourceRange, 2> {
    subs.sort_unstable();
    let mut runs: SmallVec<(u32, u32, u32), 8> = SmallVec::new(); // (base_mip, count, layer)
    for &s in subs.iter() {
        let (layer, mip) = (s / mips, s % mips);
        match runs.last_mut() {
            Some((base, count, l)) if *l == layer && *base + *count == mip => *count += 1,
            _ => runs.push((mip, 1, layer)),
        }
    }
    runs.sort_unstable();
    let mut out: SmallVec<SubresourceRange, 2> = SmallVec::new();
    for (base_mip, mip_count, layer) in runs {
        match out.last_mut() {
            Some(r)
                if r.base_mip == base_mip
                    && r.mip_count == mip_count
                    && r.base_layer + r.layer_count == layer =>
            {
                r.layer_count += 1;
            }
            _ => out.push(SubresourceRange {
                base_mip,
                mip_count,
                base_layer: layer,
                layer_count: 1,
            }),
        }
    }
    out
}
