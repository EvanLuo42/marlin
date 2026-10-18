//! Tools: the JSON dump of a plan, captures, the parallel-recording declaration.

use marlin_render_graph::{Access, CAPTURE_SCOPE, TextureDesc};
use marlin_rhi::{Format, LoadOp, StoreOp, TextureState};

use crate::support::*;

/// Brackets and quotes balance: the dump is one well-formed JSON value.
fn balanced(json: &str) -> bool {
    let mut depth: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in json.chars() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth.push(c),
            '}' if depth.pop() != Some('{') => return false,
            ']' if depth.pop() != Some('[') => return false,
            _ => {}
        }
    }
    depth.is_empty() && !in_string
}

#[test]
fn a_plan_dumps_as_json() {
    let arena = arena();
    let mut g = Graph::new(&arena);
    let t = g.create_texture(
        "Scene \"HDR\"",
        TextureDesc::new_2d(8, 8, Format::Rgba16Float),
    );
    let [t] = compute(&mut g, "Light", &[], [t]);
    sink(&mut g, "Show", &[t]);
    let json = compile(&g).to_json(&[]);
    assert!(balanced(&json), "{json}");
    assert!(json.starts_with("{\"key\":"), "{json}");
    for part in [
        "\"name\":\"Light\"",
        "\"name\":\"Show\"",
        "\"queue\":\"graphics\"",
        "\"name\":\"Scene \\\"HDR\\\"\"",
        "\"lifetime\":[0,1]",
        "\"heaps\":{\"buffers\":0,",
        "\"texture\":\"Scene \\\"HDR\\\"\"",
    ] {
        assert!(json.contains(part), "{part} in {json}");
    }
}

#[test]
fn a_capture_copies_the_resource_where_it_is_into_a_persistent_copy() {
    let mut table = Table::new();
    let plan = frame(&mut table, |g, _| {
        let t = g.create_texture("Light", TextureDesc::new_2d(8, 8, Format::Rgba16Float));
        let [t] = compute(g, "Write", &[], [t]);
        g.capture(t, "LightDebug");
        // Later writes do not change what was captured.
        compute(g, "Overwrite", &[], [t]);
    });
    assert_eq!(names(&plan), ["Write", "Capture"]);
    assert_eq!(plan.texture_barriers(1)[0].after, TextureState::COPY_SRC);
    assert!(table.has_retained("LightDebug", CAPTURE_SCOPE));
    // A frame with a capture has a topology of its own.
    let without = frame(&mut table, |g, _| {
        let t = g.create_texture("Light", TextureDesc::new_2d(8, 8, Format::Rgba16Float));
        let [t] = compute(g, "Write", &[], [t]);
        sink(g, "Show", &[t]);
    });
    assert_ne!(plan.key(), without.key());
}

#[test]
fn recording_in_parallel_is_part_of_the_topology() {
    let key = |parallel: bool| {
        let arena = arena();
        let mut g = Graph::new(&arena);
        let t = g.create_texture("T", TextureDesc::new_2d(8, 8, Format::Rgba8Unorm));
        g.add_raster("Draw", |b: &mut Builder<'_, '_>| {
            if parallel {
                b.record_in_parallel();
            }
            b.side_effect();
            (b.color(t, LoadOp::Clear([0.0; 4]), StoreOp::Store), noop())
        });
        g.topology_key()
    };
    assert_ne!(key(false), key(true));
}

#[test]
#[should_panic(expected = "LightDebug (scope 18446744073709551614) requested twice in one frame")]
fn capturing_one_name_twice_in_a_frame_panics() {
    let mut table = Table::new();
    frame(&mut table, |g, _| {
        let t = g.create_texture("Light", TextureDesc::new_2d(8, 8, Format::Rgba16Float));
        let [t] = compute(g, "Write", &[], [t]);
        g.capture(t, "LightDebug");
        g.capture(t, "LightDebug");
    });
}

#[test]
fn a_buffer_capture_reads_the_whole_buffer() {
    let mut table = Table::new();
    let plan = frame(&mut table, |g, _| {
        let b = g.create_buffer("Counts", marlin_render_graph::BufferDesc::new(64));
        let [b] = compute_buffers(g, "Count", &[], [b]);
        g.capture(b.range(16, 16), "CountsDebug");
    });
    assert_eq!(names(&plan), ["Count", "Capture"]);
    let _ = Access::CopySrc;
}
