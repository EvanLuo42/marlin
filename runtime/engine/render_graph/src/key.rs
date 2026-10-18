//! The topology key: a hash of everything compiling depends on (0007 §5.1). Closures, clear
//! values and which RHI object an import is are not part of it; import descriptions are.

use core::hash::{Hash, Hasher};

use marlin_hash::FastHasher;
use marlin_rhi::LoadOp;

use crate::backend::Backend;
use crate::graph::RenderGraph;

const fn load_kind<T>(op: &LoadOp<T>) -> u8 {
    match op {
        LoadOp::Load => 0,
        LoadOp::Clear(_) => 1,
        LoadOp::DontCare => 2,
    }
}

pub(crate) fn topology_key<B: Backend>(g: &RenderGraph<'_, B>) -> u64 {
    let mut h = FastHasher::default();
    g.textures.len().hash(&mut h);
    for t in &g.textures {
        t.desc.hash(&mut h);
        match &t.import {
            None => 0u8.hash(&mut h),
            Some(i) => {
                1u8.hash(&mut h);
                (i.initial, i.read_only, i.usage).hash(&mut h);
            }
        }
    }
    g.buffers.len().hash(&mut h);
    for b in &g.buffers {
        b.desc.hash(&mut h);
        match &b.import {
            None => 0u8.hash(&mut h),
            Some(i) => {
                1u8.hash(&mut h);
                (i.initial, i.read_only, i.usage).hash(&mut h);
            }
        }
    }
    g.passes.len().hash(&mut h);
    for p in &g.passes {
        p.name.hash(&mut h);
        (p.kind, p.queue, p.side_effect, p.builtin).hash(&mut h);
        let recs = &g.accesses[p.accesses.start as usize..p.accesses.end as usize];
        recs.len().hash(&mut h);
        for r in recs {
            (r.res, r.range, r.bytes, r.usage, r.effect).hash(&mut h);
        }
        p.colors.len().hash(&mut h);
        for c in &p.colors {
            (c.texture, c.mip, c.layer, load_kind(&c.load), c.store as u8).hash(&mut h);
        }
        match &p.depth {
            None => 0u8.hash(&mut h),
            Some(d) => {
                1u8.hash(&mut h);
                (d.texture, d.mip, d.layer, d.read_only).hash(&mut h);
                (load_kind(&d.load), d.store as u8).hash(&mut h);
                (load_kind(&d.stencil_load), d.stencil_store as u8).hash(&mut h);
            }
        }
    }
    h.finish()
}
