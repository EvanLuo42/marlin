//! Occlusion and pipeline statistics queries, and predication from their results. Hardware
//! adapter, enhanced and legacy barriers, validation on.
#![cfg(marlin_platform = "windows")]

mod support;

use marlin_rhi::{
    BufferBarrier, BufferState, BufferUsage, ColorTargetState, Format, MemoryLocation,
    PipelineStatistics, PredicateSkip, QueryKind, QueryPoolDesc,
};
use support::{
    ColorTarget, buffer, compute_pipeline, devices, read_bytes, readback, run, solid_constants,
    solid_pipeline, storage,
};

const WHITE: [f32; 4] = [1.0; 4];
/// `solid` modes: the whole 4x4 target, and a sliver that covers no sample.
const FULL: u32 = 0;
const SLIVER: u32 = 2;

fn read_u64s(buffer: &marlin_rhi::Buffer, count: usize) -> Vec<u64> {
    read_bytes(buffer, 0, count * 8)
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&c| u64::from_le_bytes(c))
        .collect()
}

#[test]
fn occlusion_queries_count_samples() {
    for device in devices() {
        let pipeline = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |_| {},
        )
        .unwrap();
        let counts = device
            .create_query_pool(&QueryPoolDesc {
                kind: QueryKind::Occlusion,
                count: 2,
            })
            .unwrap();
        let binary = device
            .create_query_pool(&QueryPoolDesc {
                kind: QueryKind::BinaryOcclusion,
                count: 2,
            })
            .unwrap();
        let out = readback(device, 32);
        let target = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        run(device, |list| {
            target.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&pipeline);
            for (i, mode) in [FULL, SLIVER].into_iter().enumerate() {
                list.push_constants(0, &solid_constants(WHITE, mode, 0.5, [0.0; 4]));
                list.begin_query(&counts, i as u32);
                list.begin_query(&binary, i as u32);
                list.draw(3, 1, 0, 0);
                list.end_query(&binary, i as u32);
                list.end_query(&counts, i as u32);
            }
            target.finish(list);
            list.resolve_queries(&counts, 0, 2, &out, 0);
            list.resolve_queries(&binary, 0, 2, &out, 16);
        });
        assert_eq!(read_u64s(&out, 4), [16, 0, 1, 0]);
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
fn pipeline_statistics_count_invocations() {
    let device = support::hardware_device();
    if !device.features().pipeline_statistics {
        return;
    }
    let pipeline = solid_pipeline(
        device,
        "solid",
        ColorTargetState::new(Format::Rgba8Unorm),
        |_| {},
    )
    .unwrap();
    let fill = compute_pipeline(device, "fill");
    // Bound, not `_`: the shader writes through the view, so the buffer must outlive the work.
    let (_storage, view) = storage(device, 64);
    let stats = device
        .create_query_pool(&QueryPoolDesc {
            kind: QueryKind::PipelineStatistics,
            count: 2,
        })
        .unwrap();
    let size = QueryKind::PipelineStatistics.result_size();
    assert_eq!(size, 112);
    let out = readback(device, 2 * size);
    let warm = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
    let target = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
    run(device, |list| {
        // Measured work follows other work in the list, as profiling scopes do: NVIDIA drivers
        // (610.88) return garbage for a graphics statistics query at the very start of a list.
        warm.begin(list, [0.0; 4]);
        list.set_graphics_pipeline(&pipeline);
        list.push_constants(0, &solid_constants(WHITE, FULL, 0.5, [0.0; 4]));
        list.draw(3, 1, 0, 0);
        warm.finish(list);
        list.set_compute_pipeline(&fill);
        list.push_constants(0, &[view.index(), 0]);
        list.begin_query(&stats, 1);
        list.dispatch(1, 1, 1);
        list.end_query(&stats, 1);
        target.begin(list, [0.0; 4]);
        list.set_graphics_pipeline(&pipeline);
        list.push_constants(0, &solid_constants(WHITE, FULL, 0.5, [0.0; 4]));
        list.begin_query(&stats, 0);
        list.draw(3, 1, 0, 0);
        list.end_query(&stats, 0);
        target.finish(list);
        list.resolve_queries(&stats, 0, 2, &out, 0);
    });
    let bytes = read_bytes(&out, 0, 2 * size as usize);
    // SAFETY: two resolved records of plain u64s, in the struct's layout.
    let records: [PipelineStatistics; 2] =
        unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast()) };
    let draw = records[0];
    assert_eq!(
        (
            draw.input_vertices,
            draw.vertex_invocations,
            draw.pixel_invocations
        ),
        (3, 3, 16),
        "{draw:?}"
    );
    assert_eq!(records[1].compute_invocations, 64, "{:?}", records[1]);
    assert_eq!(device.validation_errors(), 0);
}

/// Last pass's occlusion results decide whether this pass draws.
#[test]
fn predication_skips_occluded_draws() {
    for device in devices() {
        let pipeline = solid_pipeline(
            device,
            "solid",
            ColorTargetState::new(Format::Rgba8Unorm),
            |_| {},
        )
        .unwrap();
        let queries = device
            .create_query_pool(&QueryPoolDesc {
                kind: QueryKind::BinaryOcclusion,
                count: 2,
            })
            .unwrap();
        let predicates = buffer(
            device,
            16,
            BufferUsage::INDIRECT | BufferUsage::COPY_DST,
            MemoryLocation::GpuOnly,
        );
        let probe = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        let skipped = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        let drawn = ColorTarget::new(device, 4, Format::Rgba8Unorm, 1);
        run(device, |list| {
            probe.begin(list, [0.0; 4]);
            list.set_graphics_pipeline(&pipeline);
            for (i, mode) in [SLIVER, FULL].into_iter().enumerate() {
                list.push_constants(0, &solid_constants(WHITE, mode, 0.5, [0.0; 4]));
                list.begin_query(&queries, i as u32);
                list.draw(3, 1, 0, 0);
                list.end_query(&queries, i as u32);
            }
            probe.finish(list);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &predicates,
                before: BufferState::NONE,
                after: BufferState::COPY_DST,
            }]);
            list.resolve_queries(&queries, 0, 2, &predicates, 0);
            list.buffer_barriers(&[BufferBarrier {
                buffer: &predicates,
                before: BufferState::COPY_DST,
                after: BufferState::INDIRECT,
            }]);
            for (target, offset) in [(&skipped, 0), (&drawn, 8)] {
                target.begin(list, [0.0; 4]);
                list.set_graphics_pipeline(&pipeline);
                list.push_constants(0, &solid_constants(WHITE, FULL, 0.5, [0.0; 4]));
                list.set_predication(Some((&predicates, offset, PredicateSkip::IfZero)));
                list.draw(3, 1, 0, 0);
                list.set_predication(None);
                target.finish(list);
            }
        });
        assert_eq!(skipped.texel(1, 1), 0, "the sliver saw nothing: skipped");
        assert_eq!(
            drawn.texel(1, 1),
            0xffff_ffff,
            "the full draw was seen: drawn"
        );
        assert_eq!(device.validation_errors(), 0);
    }
}

#[test]
#[should_panic(expected = "queries are scoped")]
fn scoped_queries_are_not_written() {
    let device = support::hardware_device();
    let pool = device
        .create_query_pool(&QueryPoolDesc {
            kind: QueryKind::Occlusion,
            count: 1,
        })
        .unwrap();
    run(device, |list| list.write_timestamp(&pool, 0));
}
