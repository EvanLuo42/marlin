//! Usage, lifetimes and aliasing of transients.

use marlin_render_graph::{BufferDesc, Lifetime, Placement, TextureDesc, TextureTransition};
use marlin_rhi::{
    Access as RhiAccess, Format, HeapKind, Layout, LoadOp, PLACEMENT_ALIGNMENT, Stage, StoreOp,
    SubresourceRange, TextureState, TextureUsage,
};

use crate::support::*;

/// 256 × 256 RGBA8: 4 × 64 KiB in the recording model.
const SIDE: u32 = 256;
const SIZE: u64 = 4 * PLACEMENT_ALIGNMENT;

fn tex() -> TextureDesc {
    TextureDesc::new_2d(SIDE, SIDE, Format::Rgba8Unorm)
}

fn placed(heap: HeapKind, offset: u64, size: u64, alignment: u64) -> Option<Placement> {
    Some(Placement {
        heap,
        offset,
        size,
        alignment,
    })
}

#[test]
fn lifetimes_run_from_first_to_last_use() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", tex());
    let b = g.create_texture("B", tex());
    let [a] = compute(&mut g, "WriteA", &[], [a]);
    let [b] = compute(&mut g, "WriteB", &[a], [b]);
    sink(&mut g, "Show", &[b]);
    let plan = compile(&g);
    assert_eq!(
        plan.texture(a).lifetime,
        Some(Lifetime { first: 0, last: 1 })
    );
    assert_eq!(
        plan.texture(b).lifetime,
        Some(Lifetime { first: 1, last: 2 })
    );
}

#[test]
fn usage_is_every_way_the_frame_uses_a_texture() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let color = g.create_texture("Color", tex());
    let color = g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
        (
            b.color(color, LoadOp::Clear([0.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    sink(&mut g, "Sample", &[color]);
    let plan = compile(&g);
    assert_eq!(
        plan.texture(color).usage,
        TextureUsage::RENDER_TARGET | TextureUsage::SAMPLED
    );
    assert_eq!(
        plan.texture(color).placement.map(|p| p.heap),
        Some(HeapKind::Targets)
    );
}

#[test]
fn transients_alive_at_different_times_share_memory() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", tex());
    let b = g.create_texture("B", tex());
    let [a] = compute(&mut g, "WriteA", &[], [a]);
    sink(&mut g, "ShowA", &[a]);
    let [b] = compute(&mut g, "WriteB", &[], [b]);
    sink(&mut g, "ShowB", &[b]);
    let plan = compile(&g);
    let at_zero = placed(HeapKind::Textures, 0, SIZE, PLACEMENT_ALIGNMENT);
    assert_eq!(plan.texture(a).placement, at_zero);
    assert_eq!(plan.texture(b).placement, at_zero);
    assert_eq!(plan.heap_size(HeapKind::Textures), SIZE);
    // B's first use waits for A's last one, whose memory it takes over.
    assert_eq!(
        plan.texture_barriers(2),
        [TextureTransition {
            texture: b.index(),
            range: SubresourceRange {
                base_mip: 0,
                mip_count: 1,
                base_layer: 0,
                layer_count: 1,
            },
            before: TextureState {
                stage: Stage::COMPUTE_SHADING,
                access: RhiAccess::NONE,
                layout: Layout::Undefined,
            },
            after: TextureState::COMPUTE_WRITE,
            undefined_from: Layout::ShaderResource,
        }]
    );
}

#[test]
fn transients_alive_together_do_not_overlap() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", tex());
    let b = g.create_texture("B", tex());
    let [a] = compute(&mut g, "WriteA", &[], [a]);
    let [b] = compute(&mut g, "WriteB", &[], [b]);
    sink(&mut g, "Show", &[a, b]);
    let plan = compile(&g);
    assert_eq!(
        plan.texture(a).placement,
        placed(HeapKind::Textures, 0, SIZE, PLACEMENT_ALIGNMENT)
    );
    assert_eq!(
        plan.texture(b).placement,
        placed(HeapKind::Textures, SIZE, SIZE, PLACEMENT_ALIGNMENT)
    );
    assert_eq!(plan.heap_size(HeapKind::Textures), 2 * SIZE);
}

#[test]
fn the_heap_is_as_large_as_the_peak() {
    // A alone, then B and C (half its size each) together: the peak is A.
    let arena = arena();
    let mut g = Graph::new(&arena);
    let half = TextureDesc::new_2d(SIDE, SIDE / 2, Format::Rgba8Unorm);
    let a = g.create_texture("A", tex());
    let b = g.create_texture("B", half);
    let c = g.create_texture("C", half);
    let [a] = compute(&mut g, "WriteA", &[], [a]);
    sink(&mut g, "ShowA", &[a]);
    let [b, c] = compute(&mut g, "WriteBC", &[], [b, c]);
    sink(&mut g, "ShowBC", &[b, c]);
    let plan = compile(&g);
    assert_eq!(plan.heap_size(HeapKind::Textures), SIZE);
    let (pb, pc) = (
        plan.texture(b).placement.unwrap(),
        plan.texture(c).placement.unwrap(),
    );
    assert!(
        pb.end() <= pc.offset || pc.end() <= pb.offset,
        "{pb:?} overlaps {pc:?}"
    );
}

#[test]
fn targets_textures_and_buffers_have_their_own_heaps() {
    let arena = arena();
    let out_object = imported_buffer(256);
    let mut g = Graph::new(&arena);
    let target = g.create_texture("Target", tex());
    let storage = g.create_texture("Storage", tex());
    let buffer = g.create_buffer("Buffer", BufferDesc::new(100));
    let target = g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
        (
            b.color(target, LoadOp::Clear([0.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    let [storage] = compute(&mut g, "Fill", &[], [storage]);
    let [buffer] = compute_buffers(&mut g, "Count", &[], [buffer]);
    sink(&mut g, "Show", &[target, storage]);
    let out = g.import_buffer("Out", &out_object);
    compute_buffers(&mut g, "Use", &[buffer], [out]);
    let plan = compile(&g);
    assert_eq!(
        plan.texture(target).placement.map(|p| (p.heap, p.offset)),
        Some((HeapKind::Targets, 0))
    );
    assert_eq!(
        plan.texture(storage).placement.map(|p| (p.heap, p.offset)),
        Some((HeapKind::Textures, 0))
    );
    assert_eq!(
        plan.buffer(buffer).placement,
        placed(HeapKind::Buffers, 0, 256, 256)
    );
    assert_eq!(plan.heap_size(HeapKind::Targets), SIZE);
    assert_eq!(plan.heap_size(HeapKind::Textures), SIZE);
    assert_eq!(plan.heap_size(HeapKind::Buffers), 256);
}

#[test]
fn buffers_alias_with_their_alignment() {
    let arena = arena();
    let out_object = imported_buffer(256);
    let mut g = Graph::new(&arena);
    let a = g.create_buffer("A", BufferDesc::new(100));
    let b = g.create_buffer("B", BufferDesc::new(300));
    let [a, b] = compute_buffers(&mut g, "Write", &[], [a, b]);
    let out = g.import_buffer("Out", &out_object);
    compute_buffers(&mut g, "Read", &[a, b], [out]);
    let plan = compile(&g);
    // The larger one first.
    assert_eq!(
        plan.buffer(b).placement,
        placed(HeapKind::Buffers, 0, 512, 256)
    );
    assert_eq!(
        plan.buffer(a).placement,
        placed(HeapKind::Buffers, 512, 256, 256)
    );
    assert_eq!(plan.heap_size(HeapKind::Buffers), 768);
}

#[test]
fn imports_are_not_placed() {
    let arena = arena();
    let target = imported_texture(SIDE, SIDE, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    compute(&mut g, "Fill", &[], [out]);
    let plan = compile(&g);
    assert_eq!(plan.texture(out).placement, None);
    assert_eq!(plan.texture(out).usage, TextureUsage::STORAGE);
    assert_eq!(plan.heap_size(HeapKind::Textures), 0);
}
