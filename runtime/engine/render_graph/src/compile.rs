//! Compiling a declared graph into a [`CompiledPlan`] (0007 §4): culling, dependencies, usage,
//! lifetimes, aliasing and barriers. Single queue for now: execution order is declaration order.

use alloc::string::ToString;
use alloc::vec::Vec;

use marlin_containers::SmallVec;
use marlin_rhi::{
    Access as RhiAccess, BufferState, BufferUsage, HeapKind, Layout, Result, Stage,
    SubresourceRange, TextureState, TextureUsage,
};

use crate::access::{
    Effect, buffer_covers, is_read_only, texture_covers, texture_reads_merge, union_buffer,
    union_texture,
};
use crate::alias::{self, Item};
use crate::backend::Backend;
use crate::graph::{AccessRec, RenderGraph, Res};
use crate::plan::{
    BufferTransition, CompiledPlan, Lifetime, Placement, PlannedBuffer, PlannedPass,
    PlannedTexture, TextureFinal, TextureTransition,
};

pub(crate) fn compile<B: Backend>(
    g: &RenderGraph<'_, B>,
    device: &B::Device,
    key: u64,
) -> Result<CompiledPlan> {
    let subs = SubIndex::new(g);
    let needed = cull(g, &subs);
    let retained: Vec<u32> = (0..g.passes.len() as u32)
        .filter(|&p| needed[p as usize])
        .collect();
    let culled = (0..g.passes.len() as u32)
        .filter(|&p| !needed[p as usize])
        .collect();

    let (mut textures, mut buffers) = usage_and_lifetimes(g, &retained);
    let heap_sizes = place(g, device, &mut textures, &mut buffers)?;

    let mut states = States::new(g, &subs, &textures, &buffers);
    for (pos, &node) in retained.iter().enumerate() {
        let pass = &g.passes[node as usize];
        for rec in &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize] {
            states.access(pos as u32, rec);
        }
    }
    let (texture_barriers, buffer_barriers, ranges) = states.coalesce(retained.len());
    let passes = retained
        .iter()
        .zip(ranges)
        .map(|(&node, (tex, buf))| {
            let pass = &g.passes[node as usize];
            PlannedPass {
                node,
                name: pass.name.to_string(),
                kind: pass.kind,
                queue: pass.queue,
                texture_barriers: tex,
                buffer_barriers: buf,
            }
        })
        .collect();
    let (texture_finals, buffer_finals) = states.finals();
    let mut edges = states.edges;
    edges.sort_unstable();
    edges.dedup();
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
    })
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

fn usage_and_lifetimes<B: Backend>(
    g: &RenderGraph<'_, B>,
    retained: &[u32],
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
    let extend = |life: &mut Option<Lifetime>, pos: u32| {
        *life = Some(match *life {
            None => Lifetime {
                first: pos,
                last: pos,
            },
            Some(l) => Lifetime { last: pos, ..l },
        });
    };
    for (pos, &node) in retained.iter().enumerate() {
        let pass = &g.passes[node as usize];
        for rec in &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize] {
            match rec.res {
                Res::Texture(t) => {
                    let t = &mut textures[t as usize];
                    t.usage |= rec.usage.texture_usage();
                    extend(&mut t.lifetime, pos as u32);
                }
                Res::Buffer(b) => {
                    let b = &mut buffers[b as usize];
                    b.usage |= rec.usage.buffer_usage();
                    extend(&mut b.lifetime, pos as u32);
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

/// What the accesses since the last write were.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    /// The last access wrote (or nothing has touched the resource).
    None,
    /// Reads only; `Some` holds the barrier that started them, which later readers widen.
    Read(Option<u32>),
}

struct Track<S> {
    state: S,
    group: Group,
    last_writer: Option<u32>,
    readers: SmallVec<u32, 4>,
    touched: bool,
}

impl<S> Track<S> {
    fn new(state: S, readable: bool) -> Self {
        Self {
            state,
            group: if readable {
                Group::Read(None)
            } else {
                Group::None
            },
            last_writer: None,
            readers: SmallVec::new(),
            touched: false,
        }
    }
}

struct RawTexture {
    pass: u32,
    texture: u32,
    sub: u32,
    before: TextureState,
    after: TextureState,
    undefined_from: Layout,
    /// A transient's first use: `undefined_from` is filled in from its final layout.
    first_use: bool,
}

struct RawBuffer {
    pass: u32,
    buffer: u32,
    before: BufferState,
    after: BufferState,
}

/// Walks the retained passes in order, tracking each subresource's state.
struct States<'a, 'g, B: Backend> {
    g: &'a RenderGraph<'g, B>,
    subs: &'a SubIndex,
    textures: &'a [PlannedTexture],
    buffers: &'a [PlannedBuffer],
    tex: Vec<Track<TextureState>>,
    buf: Vec<Track<BufferState>>,
    raw_tex: Vec<RawTexture>,
    raw_buf: Vec<RawBuffer>,
    edges: Vec<(u32, u32)>,
}

impl<'a, 'g, B: Backend> States<'a, 'g, B> {
    fn new(
        g: &'a RenderGraph<'g, B>,
        subs: &'a SubIndex,
        textures: &'a [PlannedTexture],
        buffers: &'a [PlannedBuffer],
    ) -> Self {
        let mut tex = Vec::with_capacity(subs.total as usize);
        for node in &g.textures {
            let initial = node
                .import
                .as_ref()
                .map_or(TextureState::UNDEFINED, |i| i.initial);
            let readable = initial.layout != Layout::Undefined && is_read_only(initial.access);
            for _ in 0..node.desc.subresources() {
                tex.push(Track::new(initial, readable));
            }
        }
        let buf = g
            .buffers
            .iter()
            .map(|node| {
                let initial = node
                    .import
                    .as_ref()
                    .map_or(BufferState::NONE, |i| i.initial);
                Track::new(initial, is_read_only(initial.access))
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
            edges: Vec::new(),
        }
    }

    fn access(&mut self, pos: u32, rec: &AccessRec) {
        match rec.res {
            Res::Texture(t) => {
                let want = rec.usage.texture_state().expect("validated when declared");
                let transient = self.g.textures[t as usize].import.is_none();
                let mut pred_stage = None;
                let subs: SmallVec<u32, 16> = self.subs.of(self.g, t, rec).collect();
                for sub in subs {
                    let fresh = transient && !self.tex[sub as usize].touched;
                    if fresh && pred_stage.is_none() {
                        pred_stage = Some(self.alias_stage(Res::Texture(t)));
                    }
                    self.texture_sub(pos, t, sub, want, rec.effect, fresh, pred_stage);
                }
            }
            Res::Buffer(b) => {
                let want = rec.usage.buffer_state().expect("validated when declared");
                let fresh =
                    self.g.buffers[b as usize].import.is_none() && !self.buf[b as usize].touched;
                let pred = if fresh {
                    Some(self.alias_stage(Res::Buffer(b)))
                } else {
                    None
                };
                self.buffer(pos, b, want, rec.effect, pred);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn texture_sub(
        &mut self,
        pos: u32,
        texture: u32,
        sub: u32,
        want: TextureState,
        effect: Effect,
        fresh: bool,
        pred_stage: Option<Stage>,
    ) {
        let raw_index = self.raw_tex.len() as u32;
        let cur = &mut self.tex[sub as usize];
        let mut edges = SmallVec::<(u32, u32), 4>::new();
        if let Some(w) = cur.last_writer
            && w != pos
        {
            edges.push((w, pos));
        }
        let mut push = None;
        if effect.writes() {
            for &r in &cur.readers {
                if r != pos {
                    edges.push((r, pos));
                }
            }
            cur.readers.clear();
            cur.last_writer = Some(pos);
            let discard =
                fresh || effect == Effect::Overwrite || cur.state.layout == Layout::Undefined;
            let before = if fresh {
                TextureState {
                    stage: pred_stage.unwrap_or(Stage::NONE),
                    access: RhiAccess::NONE,
                    layout: Layout::Undefined,
                }
            } else if discard {
                TextureState {
                    stage: cur.state.stage,
                    access: RhiAccess::NONE,
                    layout: Layout::Undefined,
                }
            } else {
                cur.state
            };
            let undefined_from = match cur.state.layout {
                Layout::Undefined => Layout::Common,
                layout => layout,
            };
            push = Some((before, undefined_from));
            cur.state = want;
            cur.group = Group::None;
        } else {
            if cur.readers.last() != Some(&pos) {
                cur.readers.push(pos);
            }
            match cur.group {
                Group::Read(group) if !fresh && texture_reads_merge(cur.state, want) => {
                    let merged = if texture_covers(cur.state, want) {
                        cur.state
                    } else {
                        union_texture(cur.state, want)
                    };
                    if merged != cur.state {
                        match group {
                            Some(b) => {
                                let raw = &mut self.raw_tex[b as usize];
                                raw.after = union_texture(raw.after, want);
                            }
                            None => {
                                push = Some((cur.state, cur.state.layout));
                                cur.group = Group::Read(Some(raw_index));
                            }
                        }
                        cur.state = merged;
                    }
                }
                _ => {
                    let before = if fresh || cur.state.layout == Layout::Undefined {
                        TextureState {
                            stage: pred_stage.unwrap_or(cur.state.stage),
                            access: RhiAccess::NONE,
                            layout: Layout::Undefined,
                        }
                    } else {
                        cur.state
                    };
                    let undefined_from = match cur.state.layout {
                        Layout::Undefined => Layout::Common,
                        layout => layout,
                    };
                    push = Some((before, undefined_from));
                    cur.state = want;
                    cur.group = Group::Read(Some(raw_index));
                }
            }
        }
        cur.touched = true;
        let after = cur.state;
        if let Some((before, undefined_from)) = push {
            self.raw_tex.push(RawTexture {
                pass: pos,
                texture,
                sub,
                before,
                after,
                undefined_from,
                first_use: fresh,
            });
        }
        self.edges.extend(edges);
    }

    fn buffer(
        &mut self,
        pos: u32,
        buffer: u32,
        want: BufferState,
        effect: Effect,
        pred: Option<Stage>,
    ) {
        let raw_index = self.raw_buf.len() as u32;
        let fresh = pred.is_some();
        let cur = &mut self.buf[buffer as usize];
        if let Some(w) = cur.last_writer
            && w != pos
        {
            self.edges.push((w, pos));
        }
        let mut push = None;
        if effect.writes() {
            for &r in &cur.readers {
                if r != pos {
                    self.edges.push((r, pos));
                }
            }
            cur.readers.clear();
            cur.last_writer = Some(pos);
            let before = match pred {
                Some(stage) => BufferState {
                    stage,
                    access: RhiAccess::NONE,
                },
                None => cur.state,
            };
            push = Some(before);
            cur.state = want;
            cur.group = Group::None;
        } else {
            if cur.readers.last() != Some(&pos) {
                cur.readers.push(pos);
            }
            match cur.group {
                Group::Read(group) if !fresh => {
                    let merged = if buffer_covers(cur.state, want) {
                        cur.state
                    } else {
                        union_buffer(cur.state, want)
                    };
                    if merged != cur.state {
                        match group {
                            Some(b) => {
                                let raw = &mut self.raw_buf[b as usize];
                                raw.after = union_buffer(raw.after, want);
                            }
                            None => {
                                push = Some(cur.state);
                                cur.group = Group::Read(Some(raw_index));
                            }
                        }
                        cur.state = merged;
                    }
                }
                _ => {
                    push = Some(match pred {
                        Some(stage) => BufferState {
                            stage,
                            access: RhiAccess::NONE,
                        },
                        None => cur.state,
                    });
                    cur.state = want;
                    cur.group = Group::Read(Some(raw_index));
                }
            }
        }
        cur.touched = true;
        let after = cur.state;
        if let Some(before) = push {
            // Nothing to wait for and no memory to hand over: a buffer needs no barrier.
            if before.stage.is_empty() && before.access.is_empty() {
                if let Group::Read(Some(i)) = cur.group
                    && i == raw_index
                {
                    cur.group = Group::Read(None);
                }
                return;
            }
            self.raw_buf.push(RawBuffer {
                pass: pos,
                buffer,
                before,
                after,
            });
        }
    }

    /// The stages of the last uses of transients that held this one's memory before it: its
    /// first use waits for them.
    fn alias_stage(&self, res: Res) -> Stage {
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
        match res {
            Res::Texture(_) => {
                for (t, planned) in self.textures.iter().enumerate() {
                    if before(&planned.placement, &planned.lifetime) {
                        let base = self.subs.base[t] as usize;
                        let n = self.g.textures[t].desc.subresources() as usize;
                        for track in &self.tex[base..base + n] {
                            stage |= track.state.stage;
                        }
                    }
                }
            }
            Res::Buffer(_) => {
                for (b, planned) in self.buffers.iter().enumerate() {
                    if before(&planned.placement, &planned.lifetime) {
                        stage |= self.buf[b].state.stage;
                    }
                }
            }
        }
        stage
    }

    /// Merges per-subresource barriers into ranges, per pass. Returns the barriers and each
    /// pass's texture and buffer ranges.
    #[allow(clippy::type_complexity)]
    fn coalesce(
        &mut self,
        passes: usize,
    ) -> (
        Vec<TextureTransition>,
        Vec<BufferTransition>,
        Vec<(core::ops::Range<u32>, core::ops::Range<u32>)>,
    ) {
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
                        && other.texture == head.texture
                        && other.before == head.before
                        && other.after == head.after
                        && other.undefined_from == head.undefined_from
                    {
                        done[j] = true;
                        members.push(other.sub - self.subs.base[head.texture as usize]);
                    }
                }
                let mips = self.g.textures[head.texture as usize].desc.mip_levels;
                for range in ranges_of(&mut members, mips) {
                    textures.push(TextureTransition {
                        texture: head.texture,
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
                    buffer: raw.buffer,
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
