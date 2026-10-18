//! Culling and dependency edges.

use marlin_render_graph::{Access, ImportTexture, TextureDesc};
use marlin_rhi::{Format, LoadOp, StoreOp, TextureState};

use crate::support::*;

#[test]
fn passes_nobody_reads_are_culled() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let unused = g.create_texture("Unused", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "WriteA", &[], [a]);
    let [unused] = compute(&mut g, "WriteUnused", &[], [unused]);
    compute(&mut g, "ReadUnused", &[unused], []);
    sink(&mut g, "Show", &[a]);
    let plan = compile(&g);
    assert_eq!(names(&plan), ["WriteA", "Show"]);
    assert_eq!(plan.culled(), [1, 2]);
    assert_eq!(plan.texture(unused).lifetime, None);
    assert_eq!(plan.texture(unused).placement, None);
}

#[test]
fn a_graph_without_roots_culls_everything() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "Write", &[], [a]);
    compute(&mut g, "Read", &[a], []);
    let plan = compile(&g);
    assert!(plan.passes().is_empty());
}

#[test]
fn writes_to_imports_are_kept_with_their_inputs() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    let tmp = g.create_texture("Tmp", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [tmp] = compute(&mut g, "Produce", &[], [tmp]);
    compute(&mut g, "Resolve", &[tmp], [out]);
    let plan = compile(&g);
    assert_eq!(names(&plan), ["Produce", "Resolve"]);
}

#[test]
fn an_overwritten_write_is_culled() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    let out = g.add_raster("FirstClear", |b: &mut Builder<'_, '_>| {
        (
            b.color(out, LoadOp::Clear([0.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    g.add_raster("SecondClear", |b: &mut Builder<'_, '_>| {
        (
            b.color(out, LoadOp::Clear([1.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    let plan = compile(&g);
    assert_eq!(names(&plan), ["SecondClear"]);
}

#[test]
fn a_loading_write_keeps_what_it_loads() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    let out = g.add_raster("Clear", |b: &mut Builder<'_, '_>| {
        (
            b.color(out, LoadOp::Clear([0.0; 4]), StoreOp::Store),
            noop(),
        )
    });
    g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
        (b.color(out, LoadOp::Load, StoreOp::Store), noop())
    });
    let plan = compile(&g);
    assert_eq!(names(&plan), ["Clear", "Draw"]);
}

#[test]
fn read_only_imports_are_not_roots() {
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
    let tmp = g.create_texture("Tmp", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    compute(&mut g, "Blur", &[src], [tmp]);
    let plan = compile(&g);
    assert!(plan.passes().is_empty());
}

#[test]
fn side_effect_passes_are_kept() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "Write", &[], [a]);
    g.add_copy("Readback", |b: &mut Builder<'_, '_>| {
        b.side_effect();
        b.read_texture(a, Access::CopySrc);
        ((), noop())
    });
    assert_eq!(names(&compile(&g)), ["Write", "Readback"]);
}

#[test]
fn culling_follows_subresources() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture(
        "Chain",
        TextureDesc::new_2d(8, 8, Format::R32Float).with_mips(2),
    );
    let [t] = compute(&mut g, "Mip0", &[], [t.mip(0)]);
    let [t] = compute(&mut g, "Mip1", &[], [t.mip(1)]);
    sink(&mut g, "ShowMip0", &[t.mip(0)]);
    assert_eq!(names(&compile(&g)), ["Mip0", "ShowMip0"]);
}

#[test]
fn edges_are_read_after_write_write_after_read_and_write_after_write() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "W0", &[], [a]); // 0
    let [out] = compute(&mut g, "R1", &[a], [out]); // 1: reads A (RAW 0→1), writes Out
    let [a] = compute(&mut g, "W2", &[], [a]); // 2: WAW 0→2, WAR 1→2
    compute(&mut g, "R3", &[a], [out]); // 3: RAW 2→3, WAW on Out 1→3
    let plan = compile(&g);
    assert_eq!(plan.edges(), [(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)]);
}

#[test]
fn reads_in_a_row_have_no_edges_between_them() {
    let arena = arena();
    let target = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let out = g.import_texture("Out", &target);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let b_ = g.create_texture("B", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "W", &[], [a]);
    let [b_] = compute(&mut g, "R1", &[a], [b_]);
    compute(&mut g, "R2", &[a, b_], [out]);
    let plan = compile(&g);
    // R1 and R2 both read A after W; R2 also reads B after R1.
    assert_eq!(plan.edges(), [(0, 1), (0, 2), (1, 2)]);
}
