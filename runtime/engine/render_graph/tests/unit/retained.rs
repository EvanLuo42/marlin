//! History and persistent resources: which frame sees what, their states across frames, usage
//! and retirement.

use marlin_render_graph::{Access, GLOBAL_SCOPE, RETIRE_FRAMES, TextureDesc, TextureHandle};
use marlin_rhi::{Format, Layout, LoadOp, StoreOp, TextureState};

use crate::support::*;

const VIEW: u64 = 7;

fn desc() -> TextureDesc {
    TextureDesc::new_2d(8, 8, Format::Rgba16Float)
}

/// A temporal pass: reads last frame's history (if any) and writes this frame's. Returns
/// whether there was a history.
fn temporal(g: &mut Graph<'_>, desc: TextureDesc) -> bool {
    let h = g.history("Taa", VIEW, desc);
    let reads: Vec<TextureHandle<'_>> = h.prev.into_iter().collect();
    compute(g, "Resolve", &reads, [h.cur]);
    h.prev.is_some()
}

#[test]
fn a_history_has_no_prev_the_first_time_then_last_frames_cur() {
    let mut table = Table::new();
    let mut had = Vec::new();
    let first = frame(&mut table, |g, _| had.push(temporal(g, desc())));
    let second = frame(&mut table, |g, _| had.push(temporal(g, desc())));
    assert_eq!(had, [false, true]);
    // Kept although nothing in the frame reads it: the next frame does.
    assert_eq!(names(&first), ["Resolve"]);
    let barriers = second.texture_barriers(0);
    // prev: as the first frame left it; cur: the other object, never used yet.
    assert_eq!(barriers[0].before, TextureState::COMPUTE_WRITE);
    assert_eq!(barriers[0].after.layout, Layout::ShaderResource);
    assert_eq!(barriers[1].before.layout, Layout::Undefined);
}

#[test]
fn the_two_objects_take_turns() {
    let mut table = Table::new();
    for _ in 0..2 {
        frame(&mut table, |g, _| {
            temporal(g, desc());
        });
    }
    // Both objects have been written once: the third frame's prev is the second's cur.
    let third = frame(&mut table, |g, _| {
        temporal(g, desc());
    });
    let barriers = third.texture_barriers(0);
    assert_eq!(barriers[0].before, TextureState::COMPUTE_WRITE);
    // cur is the object the first frame wrote and the second read as prev.
    assert_eq!(barriers[1].before.layout, Layout::ShaderResource);
    assert_eq!(barriers[1].after, TextureState::COMPUTE_WRITE);
}

#[test]
fn a_new_description_starts_the_history_over() {
    let mut table = Table::new();
    let mut had = Vec::new();
    frame(&mut table, |g, _| had.push(temporal(g, desc())));
    let wider = TextureDesc::new_2d(16, 8, Format::Rgba16Float);
    frame(&mut table, |g, _| had.push(temporal(g, wider)));
    frame(&mut table, |g, _| had.push(temporal(g, wider)));
    assert_eq!(had, [false, false, true]);
}

#[test]
fn an_invalidated_view_has_no_prev_for_one_frame() {
    let mut table = Table::new();
    let mut had = Vec::new();
    frame(&mut table, |g, _| had.push(temporal(g, desc())));
    frame(&mut table, |g, _| {
        g.invalidate_history(VIEW);
        had.push(temporal(g, desc()));
    });
    frame(&mut table, |g, _| had.push(temporal(g, desc())));
    assert_eq!(had, [false, false, true]);
}

#[test]
fn a_frame_that_does_not_write_its_history_keeps_the_last_one() {
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        temporal(g, desc());
    });
    // Requested, but nothing writes cur.
    frame(&mut table, |g, _| {
        g.history("Taa", VIEW, desc());
    });
    let third = frame(&mut table, |g, _| {
        assert!(temporal(g, desc()));
    });
    // prev is still what the first frame wrote.
    assert_eq!(
        third.texture_barriers(0)[0].before,
        TextureState::COMPUTE_WRITE
    );
}

#[test]
fn an_unrequested_history_retires() {
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        temporal(g, desc());
    });
    for _ in 1..RETIRE_FRAMES {
        frame(&mut table, |_, _| {});
    }
    assert!(table.has_retained("Taa", VIEW));
    frame(&mut table, |_, _| {});
    assert!(!table.has_retained("Taa", VIEW));
    let mut had = true;
    frame(&mut table, |g, _| had = temporal(g, desc()));
    assert!(!had);
}

#[test]
fn a_persistent_resource_is_fresh_until_written() {
    let mut table = Table::new();
    let mut fresh = Vec::new();
    let mut accumulate = |g: &mut Graph<'_>, desc: TextureDesc| {
        let p = g.persistent("Cache", GLOBAL_SCOPE, desc);
        fresh.push(p.fresh);
        compute(g, "Accumulate", &[], [p.handle]);
    };
    for desc in [
        desc(),
        desc(),
        TextureDesc::new_2d(4, 4, Format::Rgba16Float),
    ] {
        frame(&mut table, |g, _| accumulate(g, desc));
    }
    assert_eq!(fresh, [true, false, true]);
}

#[test]
fn a_persistent_resource_keeps_its_state() {
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let p = g.persistent(
            "Counters",
            GLOBAL_SCOPE,
            marlin_render_graph::BufferDesc::new(64),
        );
        compute_buffers(g, "Count", &[], [p.handle]);
    });
    let plan = frame(&mut table, |g, _| {
        let p = g.persistent(
            "Counters",
            GLOBAL_SCOPE,
            marlin_render_graph::BufferDesc::new(64),
        );
        g.add_compute("Use", |b: &mut Builder<'_, '_>| {
            b.side_effect();
            b.read_buffer(p.handle, Access::Indirect);
            ((), noop())
        });
    });
    assert_eq!(
        plan.buffer_barriers(0)[0].before,
        marlin_rhi::BufferState::COMPUTE_WRITE
    );
}

#[test]
fn a_frame_needing_more_usage_starts_from_a_new_object() {
    let mut table = Table::new();
    let declare = |g: &mut Graph<'_>, as_target: bool| {
        let p = g.persistent("Canvas", GLOBAL_SCOPE, desc());
        if as_target {
            g.add_raster("Paint", |b: &mut Builder<'_, '_>| {
                (b.color(p.handle, LoadOp::Load, StoreOp::Store), noop())
            });
        } else {
            compute(g, "Paint", &[], [p.handle]);
        }
    };
    frame(&mut table, |g, _| declare(g, false));
    // Sampling is in the base usage: the same object.
    let sampled = frame(&mut table, |g, _| {
        let p = g.persistent("Canvas", GLOBAL_SCOPE, desc());
        sink(g, "Show", &[p.handle]);
    });
    assert_eq!(
        sampled.texture_barriers(0)[0].before,
        TextureState::COMPUTE_WRITE
    );
    // A render target it was not made as: a new object, undefined.
    let target = frame(&mut table, |g, _| declare(g, true));
    assert_eq!(
        target.texture_barriers(0)[0].before.layout,
        Layout::Undefined
    );
    // From then on it is one, and keeps its state.
    let again = frame(&mut table, |g, _| declare(g, true));
    assert_eq!(
        again.texture_barriers(0)[0].before,
        TextureState::RENDER_TARGET
    );
}

#[test]
#[should_panic(expected = "Taa (scope 7) requested twice in one frame")]
fn requesting_twice_in_a_frame_panics() {
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        g.history("Taa", VIEW, desc());
        g.history("Taa", VIEW, desc());
    });
}

#[test]
#[should_panic(expected = "need a graph with a state table")]
fn a_graph_without_a_state_table_cannot_keep_resources() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    g.history("Taa", VIEW, desc());
}
