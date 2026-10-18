//! Barriers derived from accesses: compared one by one with hand-written expectations.

use marlin_render_graph::{
    Access, BufferDesc, BufferTransition, ImportTexture, TextureDesc, TextureFinal,
    TextureTransition,
};
use marlin_rhi::{
    Access as RhiAccess, BufferState, Format, Layout, LoadOp, Stage, StoreOp, SubresourceRange,
    TextureState,
};

use crate::support::*;

const COMPUTE_READ: TextureState = TextureState {
    stage: Stage::COMPUTE_SHADING,
    access: RhiAccess::SHADER_RESOURCE,
    layout: Layout::ShaderResource,
};

const fn discarded(stage: Stage) -> TextureState {
    TextureState {
        stage,
        access: RhiAccess::NONE,
        layout: Layout::Undefined,
    }
}

const fn range(
    base_mip: u32,
    mip_count: u32,
    base_layer: u32,
    layer_count: u32,
) -> SubresourceRange {
    SubresourceRange {
        base_mip,
        mip_count,
        base_layer,
        layer_count,
    }
}

const ONE: SubresourceRange = range(0, 1, 0, 1);

fn transition(
    texture: u32,
    range: SubresourceRange,
    before: TextureState,
    after: TextureState,
    undefined_from: Layout,
) -> TextureTransition {
    TextureTransition {
        texture,
        range,
        before,
        after,
        undefined_from,
    }
}

#[test]
fn a_transient_starts_discarded_and_names_the_layout_it_ends_in() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [t] = compute(&mut g, "Write", &[], [t]);
    sink(&mut g, "Show", &[t]);
    let plan = compile(&g);
    let i = t.index();
    assert_eq!(
        plan.texture_barriers(0),
        [transition(
            i,
            ONE,
            discarded(Stage::NONE),
            TextureState::COMPUTE_WRITE,
            Layout::ShaderResource
        )]
    );
    assert_eq!(
        plan.texture_barriers(1),
        [transition(
            i,
            ONE,
            TextureState::COMPUTE_WRITE,
            COMPUTE_READ,
            Layout::UnorderedAccess
        )]
    );
}

#[test]
fn clearing_an_undefined_import_discards_from_common_and_ends_as_a_target() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    g.add_raster("Clear", |b: &mut Builder<'_, '_>| {
        (
            b.color(out, LoadOp::Clear([0.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    let plan = compile(&g);
    assert_eq!(
        plan.texture_barriers(0),
        [transition(
            out.index(),
            ONE,
            discarded(Stage::NONE),
            TextureState::RENDER_TARGET,
            Layout::Common
        )]
    );
    assert_eq!(
        plan.texture_finals(),
        [TextureFinal {
            texture: out.index(),
            range: ONE,
            state: TextureState::RENDER_TARGET,
        }]
    );
}

#[test]
fn clearing_a_used_import_waits_for_its_last_use_and_discards() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture_with(
        "Out",
        &target,
        ImportTexture {
            initial_state: Some(TextureState::PIXEL_SHADER_READ),
            read_only: false,
        },
    );
    g.add_raster("Clear", |b: &mut Builder<'_, '_>| {
        (b.color(out, LoadOp::DontCare, StoreOp::Store), noop())
    });
    let plan = compile(&g);
    assert_eq!(
        plan.texture_barriers(0),
        [transition(
            out.index(),
            ONE,
            discarded(Stage::PIXEL_SHADING),
            TextureState::RENDER_TARGET,
            Layout::ShaderResource
        )]
    );
}

#[test]
fn reads_in_a_row_share_one_barrier_widened_to_every_reader() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [t] = compute(&mut g, "Write", &[], [t]);
    g.add_raster("PixelRead", |b: &mut Builder<'_, '_>| {
        b.side_effect();
        b.read_texture(t, Access::PixelRead);
        ((), noop())
    });
    sink(&mut g, "ComputeRead", &[t]);
    let plan = compile(&g);
    let both = TextureState {
        stage: Stage::PIXEL_SHADING | Stage::COMPUTE_SHADING,
        access: RhiAccess::SHADER_RESOURCE,
        layout: Layout::ShaderResource,
    };
    assert_eq!(
        plan.texture_barriers(1),
        [transition(
            t.index(),
            ONE,
            TextureState::COMPUTE_WRITE,
            both,
            Layout::UnorderedAccess
        )]
    );
    assert_eq!(plan.texture_barriers(2), []);
}

#[test]
fn a_read_in_another_layout_needs_its_own_barrier() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [t] = compute(&mut g, "Write", &[], [t]);
    sink(&mut g, "Sample", &[t]);
    g.add_copy("Readback", |b: &mut Builder<'_, '_>| {
        b.side_effect();
        b.read_texture(t, Access::CopySrc);
        ((), noop())
    });
    let plan = compile(&g);
    assert_eq!(
        plan.texture_barriers(2),
        [transition(
            t.index(),
            ONE,
            COMPUTE_READ,
            TextureState::COPY_SRC,
            Layout::ShaderResource
        )]
    );
}

#[test]
fn storage_writes_in_a_row_are_separated() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [t] = compute(&mut g, "First", &[], [t]);
    let [t] = compute(&mut g, "Second", &[], [t]);
    sink(&mut g, "Show", &[t]);
    let plan = compile(&g);
    assert_eq!(
        plan.texture_barriers(1),
        [transition(
            t.index(),
            ONE,
            TextureState::COMPUTE_WRITE,
            TextureState::COMPUTE_WRITE,
            Layout::UnorderedAccess
        )]
    );
}

#[test]
fn a_mip_chain_transitions_one_mip_at_a_time() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let mut hzb = g.create_texture(
        "Hzb",
        TextureDesc::new_2d(8, 8, Format::R32Float).with_mips(4),
    );
    [hzb] = compute(&mut g, "Mip0", &[], [hzb.mip(0)]);
    for i in 1..4 {
        [hzb] = compute(&mut g, "Down", &[hzb.mip(i - 1)], [hzb.mip(i)]);
    }
    sink(&mut g, "Show", &[hzb]);
    let plan = compile(&g);
    let h = hzb.index();
    let mip = |i| range(i, 1, 0, 1);
    let fresh = |i| {
        transition(
            h,
            mip(i),
            discarded(Stage::NONE),
            TextureState::COMPUTE_WRITE,
            Layout::ShaderResource,
        )
    };
    let to_read = |i| {
        transition(
            h,
            mip(i),
            TextureState::COMPUTE_WRITE,
            COMPUTE_READ,
            Layout::UnorderedAccess,
        )
    };
    assert_eq!(plan.texture_barriers(0), [fresh(0)]);
    for i in 1..4 {
        assert_eq!(
            plan.texture_barriers(i as usize),
            [to_read(i - 1), fresh(i)],
            "Down {i}"
        );
    }
    // Mips 0–2 are already read in compute; only the last one changes.
    assert_eq!(plan.texture_barriers(4), [to_read(3)]);
}

#[test]
fn layers_with_the_same_transition_merge_into_ranges() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture(
        "Layers",
        TextureDesc::new_2d(8, 8, Format::Rgba8Unorm).with_layers(4),
    );
    let [t] = compute(&mut g, "Write", &[], [t]);
    sink(&mut g, "ReadLayer1", &[t.layer(1)]);
    sink(&mut g, "ReadAll", &[t]);
    let plan = compile(&g);
    let i = t.index();
    assert_eq!(
        plan.texture_barriers(0),
        [transition(
            i,
            range(0, 1, 0, 4),
            discarded(Stage::NONE),
            TextureState::COMPUTE_WRITE,
            Layout::ShaderResource
        )]
    );
    assert_eq!(
        plan.texture_barriers(1),
        [transition(
            i,
            range(0, 1, 1, 1),
            TextureState::COMPUTE_WRITE,
            COMPUTE_READ,
            Layout::UnorderedAccess
        )]
    );
    assert_eq!(
        plan.texture_barriers(2),
        [
            transition(
                i,
                range(0, 1, 0, 1),
                TextureState::COMPUTE_WRITE,
                COMPUTE_READ,
                Layout::UnorderedAccess
            ),
            transition(
                i,
                range(0, 1, 2, 2),
                TextureState::COMPUTE_WRITE,
                COMPUTE_READ,
                Layout::UnorderedAccess
            ),
        ]
    );
}

#[test]
fn an_import_already_readable_needs_no_barrier() {
    let arena = arena();
    let source = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let src = g.import_texture_with(
        "Source",
        &source,
        ImportTexture {
            initial_state: Some(TextureState::SHADER_READ),
            read_only: true,
        },
    );
    sink(&mut g, "Sample", &[src]);
    let plan = compile(&g);
    assert_eq!(plan.texture_barriers(0), []);
    assert_eq!(plan.texture_finals()[0].state, TextureState::SHADER_READ);
}

#[test]
fn an_import_read_by_a_new_stage_makes_it_visible_there() {
    let arena = arena();
    let source = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let src = g.import_texture_with(
        "Source",
        &source,
        ImportTexture {
            initial_state: Some(TextureState::PIXEL_SHADER_READ),
            read_only: true,
        },
    );
    sink(&mut g, "Sample", &[src]);
    let plan = compile(&g);
    let both = TextureState {
        stage: Stage::PIXEL_SHADING | Stage::COMPUTE_SHADING,
        ..COMPUTE_READ
    };
    assert_eq!(
        plan.texture_barriers(0),
        [transition(
            src.index(),
            ONE,
            TextureState::PIXEL_SHADER_READ,
            both,
            Layout::ShaderResource
        )]
    );
}

#[test]
fn buffer_barriers_follow_writes_and_merge_reads() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let args = g.create_buffer("Args", BufferDesc::new(64));
    let args = g.add_copy("Reset", |b: &mut Builder<'_, '_>| {
        (b.write_buffer(args, Access::Clear), noop())
    });
    let [args] = compute_buffers(&mut g, "Count", &[], [args]);
    g.add_compute("Dispatch", |b: &mut Builder<'_, '_>| {
        b.side_effect();
        b.read_buffer(args, Access::Indirect);
        ((), noop())
    });
    g.add_compute("Stats", |b: &mut Builder<'_, '_>| {
        b.side_effect();
        b.read_buffer(args, Access::ComputeRead);
        ((), noop())
    });
    let plan = compile(&g);
    let i = args.index();
    // A new buffer with nothing before it in its memory needs no barrier.
    assert_eq!(plan.buffer_barriers(0), []);
    assert_eq!(
        plan.buffer_barriers(1),
        [BufferTransition {
            buffer: i,
            before: BufferState::CLEAR_STORAGE,
            after: BufferState::COMPUTE_WRITE,
        }]
    );
    assert_eq!(
        plan.buffer_barriers(2),
        [BufferTransition {
            buffer: i,
            before: BufferState::COMPUTE_WRITE,
            after: BufferState {
                stage: Stage::EXECUTE_INDIRECT | Stage::COMPUTE_SHADING,
                access: RhiAccess::INDIRECT_ARGUMENT | RhiAccess::SHADER_RESOURCE,
            },
        }]
    );
    assert_eq!(plan.buffer_barriers(3), []);
}

#[test]
fn imported_buffers_end_in_their_last_state() {
    let arena = arena();
    let object = imported_buffer(256);
    let mut g = Graph::new(&arena);
    let buf = g.import_buffer("Counters", &object);
    compute_buffers(&mut g, "Count", &[], [buf]);
    let plan = compile(&g);
    // From NONE: nothing to wait for.
    assert_eq!(plan.buffer_barriers(0), []);
    assert_eq!(
        plan.buffer_finals(),
        [(buf.index(), BufferState::COMPUTE_WRITE)]
    );
}
