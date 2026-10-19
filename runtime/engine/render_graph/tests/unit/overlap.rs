//! Overlap scheduling (0007 §4.8): independent passes reordered into groups that run together,
//! their barriers before the group, and unordered writes that need no order between them.

use marlin_render_graph::{Access, BufferDesc, BufferHandle, TextureDesc};
use marlin_rhi::{Format, HeapKind};

use crate::support::*;

/// Two chains of three passes each, declared one after the other, each ending in a sink.
fn two_chains(g: &mut Graph<'_>) {
    for chain in ["A", "B"] {
        let t = g.create_texture(chain, TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
        let [t] = compute(g, &format!("{chain}1"), &[], [t]);
        let [t] = compute(g, &format!("{chain}2"), &[], [t]);
        sink(g, &format!("{chain}Show"), &[t]);
    }
}

#[test]
fn without_overlap_passes_run_as_declared() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    two_chains(&mut g);
    let plan = compile(&g);
    assert_eq!(names(&plan), ["A1", "A2", "AShow", "B1", "B2", "BShow"]);
}

#[test]
fn independent_chains_run_side_by_side() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    g.set_overlap(true);
    two_chains(&mut g);
    let plan = compile(&g);
    // The chains line up, level by level.
    assert_eq!(names(&plan), ["A1", "B1", "A2", "B2", "AShow", "BShow"]);
    // A group's barriers all come before its first pass.
    for pos in [1, 3] {
        assert!(plan.texture_barriers(pos).is_empty(), "{pos}");
    }
    assert_eq!(plan.texture_barriers(0).len(), 2);
    assert_eq!(plan.texture_barriers(2).len(), 2);
}

#[test]
fn passes_running_together_do_not_share_memory() {
    let heap = |overlap: bool| {
        let arena = arena();
        let mut g = Graph::new(&arena);
        g.set_overlap(overlap);
        two_chains(&mut g);
        compile(&g).heap_size(HeapKind::Textures)
    };
    // In order, B takes A's memory; side by side, both are alive at once.
    assert_eq!(heap(true), 2 * heap(false));
}

/// A pass adding into `b` with an unordered (or an ordinary) write.
fn add<'g>(
    g: &mut Graph<'g>,
    name: &str,
    b: BufferHandle<'g>,
    unordered: bool,
) -> BufferHandle<'g> {
    g.add_compute(name, |pb: &mut Builder<'_, 'g>| {
        let out = if unordered {
            pb.write_buffer_unordered(b, Access::ComputeWrite)
        } else {
            pb.write_buffer(b, Access::ComputeWrite)
        };
        (out, noop())
    })
}

/// A side-effect pass reading `b`.
fn show<'g>(g: &mut Graph<'g>, name: &str, b: BufferHandle<'g>) {
    g.add_compute(name, |pb: &mut Builder<'_, 'g>| {
        pb.side_effect();
        pb.read_buffer(b, Access::ComputeRead);
        ((), noop())
    });
}

#[test]
fn unordered_writes_need_no_order_between_them() {
    for unordered in [false, true] {
        let arena = arena();
        let mut g = Graph::new(&arena);
        g.set_overlap(true);
        let b = g.create_buffer("Counts", BufferDesc::new(256));
        let b = add(&mut g, "Add1", b, unordered);
        let b = add(&mut g, "Add2", b, unordered);
        let b = add(&mut g, "Add3", b, unordered);
        show(&mut g, "Show", b);
        let plan = compile(&g);
        let edges: Vec<(u32, u32)> = plan.edges().to_vec();
        if unordered {
            // The reader depends on every adder, the adders on nothing.
            assert_eq!(edges, [(0, 3), (1, 3), (2, 3)]);
            assert!((1..3).all(|p| plan.buffer_barriers(p).is_empty()));
        } else {
            assert_eq!(edges, [(0, 1), (1, 2), (2, 3)]);
            assert!((1..3).all(|p| plan.buffer_barriers(p).len() == 1));
        }
    }
}

#[test]
fn an_access_between_unordered_writes_orders_them() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let b = g.create_buffer("Counts", BufferDesc::new(256));
    let b = add(&mut g, "Add1", b, true);
    show(&mut g, "Read", b);
    let b = add(&mut g, "Add2", b, true);
    show(&mut g, "Show", b);
    let plan = compile(&g);
    assert_eq!(plan.edges(), [(0, 1), (0, 2), (1, 2), (2, 3)]);
    assert_eq!(plan.buffer_barriers(2).len(), 1);
}
