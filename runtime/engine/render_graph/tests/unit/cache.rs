//! Topology keys and the plan cache.

use marlin_render_graph::{Access, Lookup, PlanCache, TextureDesc};
use marlin_rhi::{ClearValue, Format, LoadOp, StoreOp};

use crate::support::*;

/// A small frame: draw into a target, blur it, show it. `variant` changes one thing at a time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Base,
    /// Another clear color: not part of the topology.
    ClearColor,
    /// Load instead of clear: part of it.
    Load,
    /// A wider target.
    Size,
    /// The blur reads in pixel shaders instead.
    Access,
    /// A pass renamed.
    Name,
    /// The import starts in another state.
    ImportState,
}

fn frame<'g>(
    g: &mut Graph<'g>,
    variant: Variant,
    out: &'g marlin_render_graph::recording::Texture,
) {
    use marlin_render_graph::ImportTexture;
    use marlin_rhi::TextureState;
    let initial_state = if variant == Variant::ImportState {
        TextureState::PRESENT
    } else {
        TextureState::UNDEFINED
    };
    let width = if variant == Variant::Size { 16 } else { 8 };
    let out = g.import_texture_with(
        "Out",
        out,
        ImportTexture {
            initial_state,
            read_only: false,
        },
    );
    let target = g.create_texture(
        "Target",
        TextureDesc::new_2d(width, 8, Format::Rgba16Float).with_clear(ClearValue::Color([0.0; 4])),
    );
    let load = match variant {
        Variant::Load => LoadOp::Load,
        Variant::ClearColor => LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
        _ => LoadOp::Clear([0.0; 4]),
    };
    let target = g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
        (b.color(target, load, StoreOp::Store), noop())
    });
    let name = if variant == Variant::Name {
        "Blur2"
    } else {
        "Blur"
    };
    let access = if variant == Variant::Access {
        Access::PixelRead
    } else {
        Access::ComputeRead
    };
    if access == Access::PixelRead {
        g.add_raster(name, |b: &mut Builder<'_, '_>| {
            b.read_texture(target, access);
            (b.color(out, LoadOp::DontCare, StoreOp::Store), noop())
        });
    } else {
        g.add_compute(name, |b: &mut Builder<'_, '_>| {
            b.read_texture(target, access);
            (b.write_texture(out, Access::ComputeWrite), noop())
        });
    }
}

fn key(variant: Variant) -> u64 {
    let arena = arena();
    let out = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut g = Graph::new(&arena);
    frame(&mut g, variant, &out);
    g.topology_key()
}

#[test]
fn the_same_topology_has_the_same_key() {
    assert_eq!(key(Variant::Base), key(Variant::Base));
    assert_eq!(key(Variant::Base), key(Variant::ClearColor));
}

#[test]
fn any_change_to_accesses_or_descriptions_changes_the_key() {
    let base = key(Variant::Base);
    for v in [
        Variant::Load,
        Variant::Size,
        Variant::Access,
        Variant::Name,
        Variant::ImportState,
    ] {
        assert_ne!(key(v), base, "{v:?}");
    }
}

#[test]
fn a_cached_plan_is_reused_while_the_topology_holds() {
    let out = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut cache = PlanCache::new();
    for (frame_index, variant) in [Variant::Base, Variant::ClearColor, Variant::Base]
        .into_iter()
        .enumerate()
    {
        let arena = arena();
        let mut g = Graph::new(&arena);
        frame(&mut g, variant, &out);
        let (plan, lookup) = cache.get_or_compile(&g, &DEVICE).unwrap();
        let expected = if frame_index == 0 {
            Lookup::Miss
        } else {
            Lookup::Hit
        };
        assert_eq!(lookup, expected, "frame {frame_index}");
        // A hit returns exactly what compiling again would.
        assert_eq!(plan, &compile(&g));
    }
    assert_eq!(cache.len(), 1);
}

#[test]
fn the_least_recently_used_plan_is_evicted() {
    let out = imported_texture(8, 8, Format::Rgba8Unorm);
    let mut cache = PlanCache::with_capacity(2);
    let mut lookup = |variant| {
        let arena = arena();
        let mut g = Graph::new(&arena);
        frame(&mut g, variant, &out);
        cache.get_or_compile(&g, &DEVICE).unwrap().1
    };
    assert_eq!(lookup(Variant::Base), Lookup::Miss);
    assert_eq!(lookup(Variant::Size), Lookup::Miss);
    assert_eq!(lookup(Variant::Base), Lookup::Hit);
    // Full: Size is the least recently used.
    assert_eq!(lookup(Variant::Load), Lookup::Miss);
    assert_eq!(lookup(Variant::Base), Lookup::Hit);
    assert_eq!(lookup(Variant::Size), Lookup::Miss);
    drop(lookup);
    assert_eq!(cache.len(), 2);
    assert!(cache.contains(key(Variant::Base)));
    assert!(cache.contains(key(Variant::Size)));
    assert!(!cache.contains(key(Variant::Load)));
}
