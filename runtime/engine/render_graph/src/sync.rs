//! Queues (0007 §4.3–4.5): which queue each pass runs on, the dependencies between passes, when
//! a pass on another queue may run relative to the graphics queue (for lifetimes), and the
//! segments each queue's passes are submitted in, with the waits between them.

use alloc::vec::Vec;
use core::ops::Range;

use marlin_rhi::{Layout, QueueType};

use crate::backend::{Backend, Queues};
use crate::graph::{AccessRec, RenderGraph, Res};

pub(crate) const GRAPHICS: u8 = 0;
pub(crate) const COMPUTE: u8 = 1;
pub(crate) const COPY: u8 = 2;

pub(crate) const fn queue_index(q: QueueType) -> u8 {
    match q {
        QueueType::Graphics => GRAPHICS,
        QueueType::Compute => COMPUTE,
        QueueType::Copy => COPY,
    }
}

pub(crate) const fn queue_type(q: u8) -> QueueType {
    match q {
        COMPUTE => QueueType::Compute,
        COPY => QueueType::Copy,
        _ => QueueType::Graphics,
    }
}

/// The queue a pass runs on: the one it asked for if the device has it, else graphics.
pub(crate) const fn resolve(asked: QueueType, queues: Queues) -> QueueType {
    match asked {
        QueueType::Compute if queues.compute => QueueType::Compute,
        QueueType::Copy if queues.copy => QueueType::Copy,
        _ => QueueType::Graphics,
    }
}

/// Dependencies between retained passes (positions), from earlier to later, sorted and unique:
/// read after write, write after read, write after write, and reads that change a texture's
/// layout after the reads before them (the transition writes the texture).
pub(crate) fn dependencies<B: Backend>(
    g: &RenderGraph<'_, B>,
    retained: &[u32],
    sub_base: &[u32],
    total: u32,
) -> Vec<(u32, u32)> {
    struct Sub {
        writer: Option<u32>,
        readers: Vec<u32>,
        layout: Layout,
    }
    let fresh = || Sub {
        writer: None,
        readers: Vec::new(),
        layout: Layout::Undefined,
    };
    let mut tex: Vec<Sub> = (0..total).map(|_| fresh()).collect();
    for (t, node) in g.textures.iter().enumerate() {
        if let Some(i) = &node.import {
            let n = node.desc.subresources() as usize;
            for s in 0..n {
                let state = if i.initial.len() == 1 {
                    i.initial[0]
                } else {
                    i.initial[s]
                };
                tex[sub_base[t] as usize + s].layout = state.layout;
            }
        }
    }
    let mut buf: Vec<Sub> = (0..g.buffers.len()).map(|_| fresh()).collect();
    let mut edges = Vec::new();
    let mut step = |sub: &mut Sub, pos: u32, rec: &AccessRec, layout: Option<Layout>| {
        if let Some(w) = sub.writer
            && w != pos
        {
            edges.push((w, pos));
        }
        let relayout = layout.is_some_and(|l| l != sub.layout);
        if rec.effect.writes() || relayout {
            for &r in &sub.readers {
                if r != pos {
                    edges.push((r, pos));
                }
            }
            sub.readers.clear();
        }
        if rec.effect.writes() {
            sub.writer = Some(pos);
        } else if sub.readers.last() != Some(&pos) {
            sub.readers.push(pos);
        }
        if let Some(l) = layout {
            sub.layout = l;
        }
    };
    for (pos, &node) in retained.iter().enumerate() {
        let pass = &g.passes[node as usize];
        for rec in &g.accesses[pass.accesses.start as usize..pass.accesses.end as usize] {
            match rec.res {
                Res::Texture(t) => {
                    let layout = rec.usage.texture_state().map(|s| s.layout);
                    let mips = g.textures[t as usize].desc.mip_levels;
                    let r = rec.range;
                    for layer in r.base_layer..r.base_layer + r.layer_count {
                        for mip in r.base_mip..r.base_mip + r.mip_count {
                            let s = sub_base[t as usize] + layer * mips + mip;
                            step(&mut tex[s as usize], pos as u32, rec, layout);
                        }
                    }
                }
                Res::Buffer(b) => step(&mut buf[b as usize], pos as u32, rec, None),
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    edges
}

/// When each pass may run, in positions of the graphics queue's order: a graphics pass at its
/// own position; a pass on another queue from the earliest its dependencies allow to the latest
/// some later work waits for it (or the end of the frame). Resources used by passes whose
/// windows overlap are alive at the same time, so their memory must not alias.
pub(crate) fn windows(queues: &[u8], edges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let n = queues.len();
    let mut preds: Vec<Vec<u32>> = (0..n).map(|_| Vec::new()).collect();
    let mut succs: Vec<Vec<u32>> = (0..n).map(|_| Vec::new()).collect();
    for &(a, b) in edges {
        if queues[a as usize] != queues[b as usize] {
            preds[b as usize].push(a);
            succs[a as usize].push(b);
        }
    }
    let mut early = alloc::vec![0u32; n];
    let mut last_on = [None::<u32>; 3];
    for p in 0..n {
        let q = queues[p];
        early[p] = if q == GRAPHICS {
            p as u32
        } else {
            let after_queue = last_on[q as usize].map_or(0, |prev| early[prev as usize]);
            preds[p]
                .iter()
                .map(|&a| early[a as usize])
                .fold(after_queue, u32::max)
        };
        last_on[q as usize] = Some(p as u32);
    }
    let end = n.saturating_sub(1) as u32;
    let mut late = alloc::vec![end; n];
    let mut next_on = [None::<u32>; 3];
    for p in (0..n).rev() {
        let q = queues[p];
        late[p] = if q == GRAPHICS {
            p as u32
        } else {
            let before_queue = next_on[q as usize].map_or(end, |next| late[next as usize]);
            succs[p]
                .iter()
                .map(|&s| {
                    if queues[s as usize] == GRAPHICS {
                        s
                    } else {
                        late[s as usize]
                    }
                })
                .fold(before_queue, u32::min)
                .max(early[p])
        };
        next_on[q as usize] = Some(p as u32);
    }
    early.into_iter().zip(late).collect()
}

/// A run of one queue's passes recorded into one command list and submitted at once: it waits
/// for other queues' segments before it starts, and signals its queue when it ends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Segment {
    pub queue: QueueType,
    /// Into [`CompiledPlan::segment_passes`](crate::CompiledPlan): positions of retained passes.
    pub(crate) passes: Range<u32>,
    /// Segments (indices, earlier in submission order) of other queues this one waits for: the
    /// latest one per queue.
    pub waits: Vec<u32>,
}

/// Splits each queue's passes into segments: a new segment starts at a pass that waits for
/// another queue (so the wait comes right before the first pass that needs it), and one ends
/// after a pass another queue waits for (so it signals as early as it can). Segments come in
/// order of their first pass, which submits every signal before the waits on it.
pub(crate) fn segments(queues: &[u8], edges: &[(u32, u32)]) -> (Vec<Segment>, Vec<u32>) {
    let n = queues.len();
    let mut cross_preds: Vec<Vec<u32>> = (0..n).map(|_| Vec::new()).collect();
    let mut signals = alloc::vec![false; n];
    for &(a, b) in edges {
        if queues[a as usize] != queues[b as usize] {
            cross_preds[b as usize].push(a);
            signals[a as usize] = true;
        }
    }
    struct Building {
        queue: u8,
        passes: Vec<u32>,
        producers: Vec<u32>,
    }
    let mut building: Vec<Building> = Vec::new();
    let mut open: [Option<usize>; 3] = [None; 3];
    let mut segment_of = alloc::vec![0u32; n];
    for p in 0..n {
        let q = queues[p];
        let seg = match open[q as usize] {
            Some(s) if cross_preds[p].is_empty() => s,
            _ => {
                building.push(Building {
                    queue: q,
                    passes: Vec::new(),
                    producers: Vec::new(),
                });
                building.len() - 1
            }
        };
        building[seg].passes.push(p as u32);
        building[seg].producers.extend(&cross_preds[p]);
        segment_of[p] = seg as u32;
        open[q as usize] = if signals[p] { None } else { Some(seg) };
    }
    let mut out = Vec::with_capacity(building.len());
    let mut flat = Vec::with_capacity(n);
    for b in &building {
        let start = flat.len() as u32;
        flat.extend(&b.passes);
        let mut latest = [None::<u32>; 3];
        for &producer in &b.producers {
            let seg = segment_of[producer as usize];
            let pq = queues[producer as usize] as usize;
            latest[pq] = Some(latest[pq].map_or(seg, |s| s.max(seg)));
        }
        let mut waits: Vec<u32> = latest.into_iter().flatten().collect();
        waits.sort_unstable();
        out.push(Segment {
            queue: queue_type(b.queue),
            passes: start..flat.len() as u32,
            waits,
        });
    }
    (out, flat)
}
