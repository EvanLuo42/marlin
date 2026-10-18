//! Async compute and the copy queue: queue choice, segments and waits, lifetimes across queues,
//! barriers on the consumer's queue.

use marlin_render_graph::recording::Device;
use marlin_render_graph::{Access, BufferDesc, CompiledPlan, TextureDesc, TextureHandle};
use marlin_rhi::{Access as RhiAccess, Format, HeapKind, Layout, QueueType, Stage, TextureState};

use crate::support::*;

/// A compute pass on the compute queue.
fn async_pass<'g, const N: usize>(
    g: &mut Graph<'g>,
    name: &str,
    reads: &[TextureHandle<'g>],
    writes: [TextureHandle<'g>; N],
) -> [TextureHandle<'g>; N] {
    g.add_compute(name, |b: &mut Builder<'_, 'g>| {
        b.async_compute();
        for &r in reads {
            b.read_texture(r, Access::ComputeRead);
        }
        (
            writes.map(|w| b.write_texture(w, Access::ComputeWrite)),
            noop(),
        )
    })
}

fn on(device: &Device, g: &Graph<'_>) -> CompiledPlan {
    g.compile(device).unwrap()
}

fn queues(plan: &CompiledPlan) -> Vec<QueueType> {
    plan.passes().iter().map(|p| p.queue).collect()
}

/// Segments as (queue, passes, waits).
fn segments(plan: &CompiledPlan) -> Vec<(QueueType, Vec<u32>, Vec<u32>)> {
    plan.segments()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.queue, plan.segment_passes(i).to_vec(), s.waits.clone()))
        .collect()
}

const G: QueueType = QueueType::Graphics;
const C: QueueType = QueueType::Compute;

#[test]
fn async_passes_run_on_the_compute_queue_where_there_is_one() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [a] = async_pass(&mut g, "Async", &[], [a]);
    sink(&mut g, "Show", &[a]);
    assert_eq!(queues(&on(&Device::ASYNC, &g)), [C, G]);
    let single = on(&Device::GRAPHICS_ONLY, &g);
    assert_eq!(queues(&single), [G, G]);
    assert_eq!(segments(&single), [(G, vec![0, 1], vec![])]);
    // A graph asked to stay on one queue does, on any device.
    g.set_single_queue(true);
    assert_eq!(queues(&on(&Device::ASYNC, &g)), [G, G]);
}

#[test]
fn waits_come_right_before_the_first_pass_that_needs_them() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::R32Float));
    let b = g.create_texture("B", TextureDesc::new_2d(8, 8, Format::R32Float));
    let c = g.create_texture("C", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [a] = compute(&mut g, "MakeA", &[], [a]); // 0, graphics
    let [b] = async_pass(&mut g, "Light", &[a], [b]); // 1, compute: waits for 0
    let [c] = compute(&mut g, "Raster", &[], [c]); // 2, graphics: does not wait
    sink(&mut g, "Compose", &[b, c]); // 3, graphics: waits for 1
    let plan = on(&Device::ASYNC, &g);
    assert_eq!(
        segments(&plan),
        [
            (G, vec![0], vec![]),
            (C, vec![1], vec![0]),
            (G, vec![2], vec![]),
            (G, vec![3], vec![1]),
        ]
    );
}

#[test]
fn waits_on_one_queue_merge_into_the_latest() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::R32Float));
    let b = g.create_texture("B", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [a] = async_pass(&mut g, "One", &[], [a]); // 0
    let [b] = async_pass(&mut g, "Two", &[], [b]); // 1
    sink(&mut g, "Use", &[a, b]); // 2: waits for both, one wait on the later
    let plan = on(&Device::ASYNC, &g);
    assert_eq!(
        segments(&plan),
        [
            (C, vec![0], vec![]),
            (C, vec![1], vec![]),
            (G, vec![2], vec![1]),
        ]
    );
}

#[test]
fn an_async_pass_without_dependencies_may_run_from_the_start() {
    // B is used by an async pass declared after A's graphics passes, but nothing orders it
    // after them: B must not take A's memory. On one queue it does.
    let declare = |g: &mut Graph<'_>| {
        let a = g.create_texture("A", TextureDesc::new_2d(64, 64, Format::Rgba8Unorm));
        let b = g.create_texture("B", TextureDesc::new_2d(64, 64, Format::Rgba8Unorm));
        let [a] = compute(g, "WriteA", &[], [a]);
        sink(g, "ShowA", &[a]);
        let [b] = async_pass(g, "WriteB", &[], [b]);
        sink(g, "ShowB", &[b]);
    };
    let arena = arena();
    let mut g = Graph::new(&arena);
    declare(&mut g);
    let spread = on(&Device::ASYNC, &g);
    let single = on(&Device::GRAPHICS_ONLY, &g);
    let one = 64 * 64 * 4u64;
    let one = one.next_multiple_of(marlin_rhi::PLACEMENT_ALIGNMENT);
    assert_eq!(single.heap_size(HeapKind::Textures), one);
    assert_eq!(spread.heap_size(HeapKind::Textures), 2 * one);
}

#[test]
fn a_consumer_on_another_queue_changes_the_layout_after_its_wait() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [a] = compute(&mut g, "Write", &[], [a]);
    let b = g.create_texture("B", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [b] = async_pass(&mut g, "Read", &[a], [b]);
    sink(&mut g, "Show", &[b]);
    let plan = on(&Device::ASYNC, &g);
    let read = plan
        .texture_barriers(1)
        .iter()
        .find(|t| t.texture == a.index())
        .copied()
        .unwrap();
    assert_eq!(
        read.before,
        TextureState {
            stage: Stage::ALL,
            access: RhiAccess::NONE,
            layout: Layout::UnorderedAccess,
        }
    );
    assert_eq!(read.after.layout, Layout::ShaderResource);
}

#[test]
fn buffers_handed_between_queues_need_no_barrier() {
    let out_object = imported_buffer(256);
    let arena = arena();
    let mut g = Graph::new(&arena);
    let out = g.import_buffer("Out", &out_object);
    let a = g.create_buffer("A", BufferDesc::new(256));
    let a = g.add_compute("Produce", |b: &mut Builder<'_, '_>| {
        b.async_compute();
        (b.write_buffer(a, Access::ComputeWrite), noop())
    });
    compute_buffers(&mut g, "Consume", &[a], [out]);
    let plan = on(&Device::ASYNC, &g);
    let names: Vec<_> = plan.buffer_barriers(1).iter().map(|b| b.buffer).collect();
    assert!(!names.contains(&a.index()), "{:?}", plan.buffer_barriers(1));
}

#[test]
fn a_transition_on_one_queue_waits_for_reads_on_another() {
    // Read on graphics, then copied out on the copy queue: the copy's layout change must wait
    // for the read.
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::R32Float));
    let [a] = compute(&mut g, "Write", &[], [a]); // 0
    sink(&mut g, "Sample", &[a]); // 1
    g.add_copy("Copy", |b: &mut Builder<'_, '_>| {
        b.copy_queue();
        b.side_effect();
        b.read_texture(a, Access::CopySrc);
        ((), noop())
    }); // 2
    let plan = on(&Device::ASYNC, &g);
    assert_eq!(plan.passes()[2].queue, QueueType::Copy);
    assert!(plan.edges().contains(&(1, 2)), "{:?}", plan.edges());
}

#[test]
#[should_panic(expected = "pass Reset: the copy queue cannot clear")]
fn the_copy_queue_cannot_clear() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_buffer("A", BufferDesc::new(64));
    g.add_copy("Reset", |b: &mut Builder<'_, '_>| {
        b.copy_queue();
        (b.write_buffer(a, Access::Clear), noop())
    });
}
