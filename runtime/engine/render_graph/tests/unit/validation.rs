//! Development checks: misuse panics naming the pass and the resource (0007 §10.2).

use marlin_render_graph::{Access, ImportTexture, TextureDesc, recording};
use marlin_rhi::{Format, LoadOp, StoreOp, TextureState, TextureUsage};

use crate::support::*;

#[test]
#[should_panic(expected = "pass Late: A used at version 1, but version 2 replaced it")]
fn using_an_old_version_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [v1] = compute(&mut g, "First", &[], [a]);
    let [_v2] = compute(&mut g, "Second", &[], [v1]);
    sink(&mut g, "Late", &[v1]);
}

#[test]
#[should_panic(expected = "pass Write: writes Source, imported read-only")]
fn writing_a_read_only_import_panics() {
    let arena = arena();
    let object = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    let src = g.import_texture_with(
        "Source",
        &object,
        ImportTexture {
            initial_state: Some(TextureState::SHADER_READ),
            read_only: true,
        },
    );
    compute(&mut g, "Write", &[], [src]);
}

#[test]
#[should_panic(expected = "pass Both: declares A as ComputeRead and as ComputeWrite")]
fn reading_and_writing_the_same_subresource_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "Init", &[], [a]);
    compute(&mut g, "Both", &[a], [a]);
}

#[test]
fn reading_one_mip_and_writing_another_is_fine() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture(
        "T",
        TextureDesc::new_2d(8, 8, Format::R32Float).with_mips(2),
    );
    let [t] = compute(&mut g, "Mip0", &[], [t.mip(0)]);
    let [t] = compute(&mut g, "Mip1", &[t.mip(0)], [t.mip(1)]);
    sink(&mut g, "Show", &[t]);
    assert_eq!(compile(&g).passes().len(), 3);
}

#[test]
fn writing_two_mips_in_one_pass_is_fine() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture(
        "T",
        TextureDesc::new_2d(8, 8, Format::R32Float).with_mips(2),
    );
    let [t, _] = compute(&mut g, "Both", &[], [t.mip(0), t.mip(1)]);
    sink(&mut g, "Show", &[t]);
    assert_eq!(compile(&g).passes().len(), 2);
}

#[test]
#[should_panic(expected = "pass Read: reads A before any pass wrote it")]
fn reading_an_unwritten_transient_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    sink(&mut g, "Read", &[a]);
}

#[test]
#[should_panic(
    expected = "pass Mixed: reads A as ComputeRead and as CopySrc, which need different layouts"
)]
fn reads_needing_different_layouts_in_one_pass_panic() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "Init", &[], [a]);
    g.add_raster("Mixed", |b: &mut Builder<'_, '_>| {
        b.read_texture(a, Access::ComputeRead);
        b.read_texture(a, Access::CopySrc);
        ((), noop())
    });
}

#[test]
#[should_panic(expected = "pass Fill: ComputeWrite access to Sampled, which was created without")]
fn accessing_an_import_beyond_its_usage_panics() {
    let arena = arena();
    let object = recording::Texture(marlin_rhi::TextureDesc::new_2d(
        8,
        8,
        Format::Rgba8Unorm,
        TextureUsage::SAMPLED,
    ));
    let mut g = Graph::new(&arena);
    let t = g.import_texture("Sampled", &object);
    compute(&mut g, "Fill", &[], [t]);
}

#[test]
#[should_panic(expected = "pass Draw: a Compute pass cannot use A as PixelRead")]
fn an_access_the_pass_kind_cannot_make_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    let [a] = compute(&mut g, "Init", &[], [a]);
    g.add_compute("Draw", |b: &mut Builder<'_, '_>| {
        b.read_texture(a, Access::PixelRead);
        ((), noop())
    });
}

#[test]
#[should_panic(expected = "the color attachment A must be one mip and one layer")]
fn an_attachment_of_several_mips_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture(
        "A",
        TextureDesc::new_2d(8, 8, Format::Rgba8Unorm).with_mips(2),
    );
    g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
        (b.color(a, LoadOp::Clear([0.0; 4]), StoreOp::Store), noop())
    });
}

#[test]
#[should_panic(expected = "subresource 2..+1 outside a handle covering 1")]
fn a_mip_outside_the_handle_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture(
        "A",
        TextureDesc::new_2d(8, 8, Format::Rgba8Unorm).with_mips(4),
    );
    let _ = a.mip(1).mip(2);
}

#[test]
#[should_panic(expected = "pass Write: ComputeRead only reads; declare it with read_texture")]
fn declaring_a_read_as_a_write_panics() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let a = g.create_texture("A", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
    g.add_compute("Write", |b: &mut Builder<'_, '_>| {
        (b.write_texture(a, Access::ComputeRead), noop())
    });
}
