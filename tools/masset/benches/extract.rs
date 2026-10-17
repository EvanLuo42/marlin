//! The indexing fast path, the full parser and the canonical writer on a synthetic 50,000-object
//! file; throughput is in bytes of the source text.
//! `cargo xtask test --tier bench -p marlin-masset`, or
//! `cargo bench -p marlin-masset --bench extract -- [filter]`.

use marlin_bench::Bench;
use marlin_masset::{extract, new_uuid, parse, write};

const OBJECTS: usize = 50_000;

/// A synthetic project file with `objects` objects of a few links and files each.
fn big_file(objects: usize) -> String {
    let mut text = String::from("!Version 1\n");
    for i in 0..objects {
        text.push_str(&format!(
            "\nMeshInstance\n{{\n    !Uuid = \"{}\"\n    !Name = \"Instance{i}\"\n    Mesh = <props/rock_{}:Rock #{}>\n    Material = <materials/stone:Stone #{}>\n    Transform = {{ Translation = ({i}.5, 2.0, -3.25), Rotation = (0.0, 0.0, 0.0, 1.0), Scale = (1.0, 1.0, 1.0) }}\n    Tags = [\"rock\", \"static\", \"lod{}\"]\n    Lightmap = `source:lightmaps/l{i}.exr`\n}}\n",
            new_uuid(),
            i % 50,
            new_uuid(),
            new_uuid(),
            i % 4,
        ));
    }
    text
}

fn throughput() {
    let text = big_file(OBJECTS);
    let bytes = text.len() as u64;
    let doc = parse(&text).unwrap();
    assert_eq!(extract(&text).unwrap().objects.len(), OBJECTS);
    assert_eq!(doc.objects.len(), OBJECTS);
    assert!(write(&doc).len() > text.len() / 2);

    // One call per sample, the result dropped outside the timing.
    Bench::new("extract")
        .throughput_bytes(bytes)
        .run_with_setup(|| (), |()| extract(&text).unwrap());
    Bench::new("parse")
        .throughput_bytes(bytes)
        .run_with_setup(|| (), |()| parse(&text).unwrap());
    Bench::new("write")
        .throughput_bytes(bytes)
        .run_with_setup(|| (), |()| write(&doc));
}

marlin_bench::main!(throughput);
