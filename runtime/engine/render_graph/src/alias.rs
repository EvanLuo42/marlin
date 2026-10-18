//! Memory for transients: first-fit over lifetimes, so resources that are never alive at once
//! share bytes (0007 §4.5).

use alloc::vec::Vec;

use marlin_rhi::{HeapKind, MemoryRequirements};

use crate::plan::{Lifetime, Placement};

pub(crate) struct Item {
    pub lifetime: Lifetime,
    pub req: MemoryRequirements,
    pub heap: HeapKind,
}

const fn heap_slot(kind: HeapKind) -> usize {
    match kind {
        HeapKind::Buffers => 0,
        HeapKind::Textures => 1,
        HeapKind::Targets | HeapKind::Any => 2,
    }
}

/// Places every item; returns their placements (in `items` order) and each heap's size.
///
/// Larger resources go first, which keeps small ones from fragmenting the heap; ties go by
/// lifetime start and then by order, so the result is deterministic. Each item takes the lowest
/// aligned offset clear of every placed item whose lifetime overlaps its own.
pub(crate) fn place(items: &[Item]) -> (Vec<Placement>, [u64; 3]) {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&items[a], &items[b]);
        heap_slot(x.heap)
            .cmp(&heap_slot(y.heap))
            .then(y.req.size.cmp(&x.req.size))
            .then(x.lifetime.first.cmp(&y.lifetime.first))
            .then(a.cmp(&b))
    });
    let mut placed: Vec<Option<Placement>> = alloc::vec![None; items.len()];
    let mut sizes = [0u64; 3];
    let mut busy: Vec<(u64, u64)> = Vec::new();
    for &i in &order {
        let item = &items[i];
        busy.clear();
        for &j in &order {
            if let Some(p) = placed[j]
                && p.heap == item.heap
                && items[j].lifetime.overlaps(item.lifetime)
            {
                busy.push((p.offset, p.end()));
            }
        }
        busy.sort_unstable();
        let mut offset = 0;
        for &(start, end) in &busy {
            if offset + item.req.size <= start {
                break;
            }
            offset = offset.max(end.next_multiple_of(item.req.alignment));
        }
        let p = Placement {
            heap: item.heap,
            offset,
            size: item.req.size,
            alignment: item.req.alignment,
        };
        let slot = &mut sizes[heap_slot(item.heap)];
        *slot = (*slot).max(p.end());
        placed[i] = Some(p);
    }
    (
        placed
            .into_iter()
            .map(|p| p.expect("placed above"))
            .collect(),
        sizes,
    )
}
