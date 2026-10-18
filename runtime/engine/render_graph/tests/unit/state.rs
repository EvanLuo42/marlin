//! The cross-frame state table, present and readback passes.

use marlin_render_graph::{Access, ImportTexture, StateTable, TextureDesc, recording};
use marlin_rhi::{
    Access as RhiAccess, BufferState, Format, Layout, LoadOp, Stage, StoreOp, TextureState,
    TextureUsage,
};

use crate::support::*;

type Table = StateTable<marlin_render_graph::recording::Recording>;

/// Runs one frame over `table`: declares with `declare`, compiles, records the imports' states.
/// `&'g &'o ()` lets `declare` import objects that live for `'o` (it implies `'o: 'g`).
fn frame<'o>(
    table: &mut Table,
    declare: impl for<'g> FnOnce(&mut Graph<'g>, &'g &'o ()),
) -> marlin_render_graph::CompiledPlan {
    let arena = arena();
    let (plan, states) = {
        let mut g = Graph::with_states(&arena, table);
        declare(&mut g, &&());
        let plan = compile(&g);
        let states = g.import_states(&plan);
        (plan, states)
    };
    table.end_frame(states);
    plan
}

fn mipped(mips: u32) -> recording::Texture {
    let mut desc = marlin_rhi::TextureDesc::new_2d(
        8,
        8,
        Format::R32Float,
        TextureUsage::SAMPLED | TextureUsage::STORAGE | TextureUsage::COPY_SRC,
    );
    desc.mip_levels = mips;
    recording::Texture(desc)
}

const COMPUTE_READ: TextureState = TextureState {
    stage: Stage::COMPUTE_SHADING,
    access: RhiAccess::SHADER_RESOURCE,
    layout: Layout::ShaderResource,
};

#[test]
fn an_import_starts_where_the_last_frame_left_it() {
    let object = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "Write", &[], [t]);
    });
    assert_eq!(table.frame(), 1);
    let plan = frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        sink(g, "Read", &[t]);
    });
    assert_eq!(
        plan.texture_barriers(0)[0].before,
        TextureState::COMPUTE_WRITE
    );
    assert_eq!(plan.texture_barriers(0)[0].after, COMPUTE_READ);
}

#[test]
fn subresources_keep_their_own_states() {
    let object = mipped(2);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "WriteMip0", &[], [t.mip(0)]);
    });
    let key = core::ptr::from_ref(&object) as u64;
    assert_eq!(
        table.texture_states(key),
        Some(&[TextureState::COMPUTE_WRITE, TextureState::UNDEFINED][..])
    );
    let plan = frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        sink(g, "ReadMip0", &[t.mip(0)]);
    });
    assert_eq!(
        plan.texture_barriers(0)[0].before,
        TextureState::COMPUTE_WRITE
    );
}

#[test]
fn an_explicit_initial_state_wins_over_the_table() {
    let object = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "Write", &[], [t]);
    });
    let plan = frame(&mut table, |g, _| {
        let t = g.import_texture_with(
            "T",
            &object,
            ImportTexture {
                initial_state: Some(TextureState::PIXEL_SHADER_READ),
                read_only: false,
            },
        );
        sink(g, "Read", &[t]);
    });
    assert_eq!(
        plan.texture_barriers(0)[0].before,
        TextureState::PIXEL_SHADER_READ
    );
}

#[test]
fn a_forgotten_object_starts_undefined() {
    let object = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "Write", &[], [t]);
    });
    table.forget_texture(&object);
    assert!(table.is_empty());
    let plan = frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "Write", &[], [t]);
    });
    assert_eq!(plan.texture_barriers(0)[0].before.layout, Layout::Undefined);
    assert_eq!(plan.texture_barriers(0)[0].undefined_from, Layout::Common);
}

#[test]
fn frames_that_do_not_use_an_import_keep_its_state() {
    let object = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        compute(g, "Write", &[], [t]);
    });
    // Imported but culled, then not imported at all.
    frame(&mut table, |g, _| {
        let t = g.import_texture("T", &object);
        g.add_compute("Unused", |b: &mut Builder<'_, '_>| {
            b.read_texture(t, Access::ComputeRead);
            ((), noop())
        });
    });
    frame(&mut table, |_, _| {});
    let key = core::ptr::from_ref(&object) as u64;
    assert_eq!(
        table.texture_states(key),
        Some(&[TextureState::COMPUTE_WRITE][..])
    );
}

#[test]
fn entries_unused_for_long_are_dropped() {
    let object = imported_buffer(64);
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let b = g.import_buffer("B", &object);
        compute_buffers(g, "Write", &[], [b]);
    });
    let key = core::ptr::from_ref(&object) as u64;
    assert_eq!(table.buffer_state(key), Some(BufferState::COMPUTE_WRITE));
    for _ in 1..Table::TTL {
        table.end_frame(Default::default());
    }
    assert_eq!(table.buffer_state(key), Some(BufferState::COMPUTE_WRITE));
    table.end_frame(Default::default());
    assert_eq!(table.buffer_state(key), None);
}

#[test]
fn a_presented_image_ends_in_present_and_starts_there_next_time() {
    let image = imported_texture(8, 8, Format::Bgra8UnormSrgb);
    let mut table = Table::new();
    fn draw<'g>(g: &mut Graph<'g>, image: &'g recording::Texture) {
        let t = g.import_texture("Swapchain", image);
        let t = g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
            (b.color(t, LoadOp::Clear([0.0; 4]), StoreOp::Store), noop())
        });
        g.present(t);
    }
    let first = frame(&mut table, |g, _| draw(g, &image));
    assert_eq!(names(&first), ["Draw", "Present"]);
    assert_eq!(first.texture_barriers(1)[0].after, TextureState::PRESENT);
    assert_eq!(first.texture_finals()[0].state, TextureState::PRESENT);
    let second = frame(&mut table, |g, _| draw(g, &image));
    let clear = second.texture_barriers(0)[0];
    assert_eq!(clear.before.layout, Layout::Undefined);
    assert_eq!(clear.undefined_from, Layout::Present);
    assert_eq!(first.key(), first.key());
    // The first frame started undefined, the second from Present: two topologies.
    assert_ne!(first.key(), second.key());
}

#[test]
fn a_readback_is_a_copy_the_frame_keeps() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [t] = compute(&mut g, "Write", &[], [t]);
    g.readback_texture(t);
    let buf = g.create_buffer("B", marlin_render_graph::BufferDesc::new(64));
    let [buf] = compute_buffers(&mut g, "Count", &[], [buf]);
    let first = g.readback(buf);
    let plan = compile(&g);
    assert_eq!(names(&plan), ["Write", "Readback", "Count", "Readback"]);
    assert_eq!(plan.texture_barriers(1)[0].after, TextureState::COPY_SRC);
    assert_eq!(plan.buffer_barriers(3)[0].after, BufferState::COPY_SRC);
    assert_ne!(first, g.readback(buf));
}
