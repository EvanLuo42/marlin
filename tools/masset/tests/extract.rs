//! The indexing fast path agrees with the full parser.

use std::time::Instant;

use marlin_asset::Uuid;
use marlin_masset::{Extract, NodeKind, extract, new_uuid, parse};

const TEXT: &str = r#"!Version 1

EnvironmentProbe
{
    !Uuid = "deb8916d-3350-78d5-bf61-c2afbe805f67"
    !Name = "Probe1"
    Depth = <level/probe1_depth:Probe1Depth #a1c3916d-3350-78d5-bf61-c2afbe805f00>
    Nested = { Inner = [<:Probe1Albedo #01832501-7918-7ee4-b177-2f200aab91b3>, <other:Thing>] }
    Name = "not the !Name"
}

Texture
{
    !Uuid = "01832501-7918-7ee4-b177-2f200aab91b3"
    !Name = "Probe\u{41}lbedo"
    Source = `source:level/probe1_albedo.png`
    Settings = Fancy { Lut = `source:luts/warm.cube` }
}
"#;

#[test]
fn extracts_objects_links_and_files() {
    let e = extract(TEXT).unwrap();
    assert_eq!(e.version, 1);
    assert_eq!(e.objects.len(), 2);

    let probe = &e.objects[0];
    assert_eq!(probe.type_name, "EnvironmentProbe");
    assert_eq!(probe.name, "Probe1");
    assert_eq!(
        probe.uuid.to_string(),
        "deb8916d-3350-78d5-bf61-c2afbe805f67"
    );
    let links: Vec<(&str, &str, Option<String>)> = probe
        .links
        .iter()
        .map(|l| (l.path, l.name, l.uuid.map(|u| u.to_string())))
        .collect();
    assert_eq!(
        links,
        [
            (
                "level/probe1_depth",
                "Probe1Depth",
                Some("a1c3916d-3350-78d5-bf61-c2afbe805f00".into())
            ),
            (
                "",
                "Probe1Albedo",
                Some("01832501-7918-7ee4-b177-2f200aab91b3".into())
            ),
            ("other", "Thing", None),
        ]
    );
    assert!(probe.files.is_empty());

    let texture = &e.objects[1];
    assert_eq!(texture.name, "ProbeAlbedo", "escapes are decoded");
    let files: Vec<(&str, &str)> = texture.files.iter().map(|f| (f.scheme, f.path)).collect();
    assert_eq!(
        files,
        [
            ("source", "level/probe1_albedo.png"),
            ("source", "luts/warm.cube")
        ]
    );
}

#[test]
fn agrees_with_the_parser() {
    let doc = parse(TEXT).unwrap();
    let e = extract(TEXT).unwrap();
    for (object, extracted) in doc.objects.iter().zip(&e.objects) {
        assert_eq!(object.type_name, extracted.type_name);
        assert_eq!(object.uuid, extracted.uuid);
        assert_eq!(object.name, extracted.name);
        assert_eq!(object.span, extracted.span);
        let mut links = 0;
        let mut files = 0;
        fn walk(kind: &NodeKind, links: &mut usize, files: &mut usize) {
            match kind {
                NodeKind::Link(_) => *links += 1,
                NodeKind::File(_) => *files += 1,
                NodeKind::Call(_, items) | NodeKind::Tuple(items) | NodeKind::List(items) => {
                    items.iter().for_each(|n| walk(&n.kind, links, files));
                }
                NodeKind::Record(_, attrs) => {
                    attrs.iter().for_each(|a| walk(&a.value.kind, links, files));
                }
                _ => {}
            }
        }
        for attr in &object.attrs {
            walk(&attr.value.kind, &mut links, &mut files);
        }
        assert_eq!(links, extracted.links.len());
        assert_eq!(files, extracted.files.len());
    }
}

#[test]
fn structural_errors_are_reported() {
    for (text, message) in [
        ("Thing {}", "`!Version`"),
        ("!Version 1\nThing { !Name = \"A\" }", "no `!Uuid`"),
        (
            "!Version 1\nThing { !Uuid = \"00000000-0000-0000-0000-000000000001\" }",
            "no `!Name`",
        ),
        (
            "!Version 1\nThing { !Uuid = \"00000000-0000-0000-0000-000000000001\" !Name = \"A\" X = (] }",
            "mismatched",
        ),
        (
            "!Version 1\nThing { !Uuid = \"00000000-0000-0000-0000-000000000001\" !Name = \"A\"",
            "not closed",
        ),
        ("!Version 1\n= {}", "object type name"),
    ] {
        let error = extract(text).expect_err(text);
        assert!(error.message.contains(message), "{text}: {error}");
    }
    assert_eq!(
        extract("!Version 1\n").unwrap(),
        Extract {
            version: 1,
            objects: Vec::new()
        }
    );
}

#[test]
fn new_uuids_are_version_7_and_increasing() {
    let ids: Vec<Uuid> = (0..10_000).map(|_| new_uuid()).collect();
    assert!(ids.iter().all(|id| id.version() == 7));
    assert!(
        ids.iter().all(|id| (id.as_u128() >> 62) & 0b11 == 0b10),
        "RFC 9562 variant"
    );
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
    let text = ids[0].to_string();
    assert_eq!(text.parse::<Uuid>().unwrap(), ids[0]);
    assert_eq!(text.as_bytes()[14], b'7');
}

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

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn bench_extract_and_parse() {
    let text = big_file(50_000);
    let mb = text.len() as f64 / (1024.0 * 1024.0);
    let start = Instant::now();
    let e = extract(&text).unwrap();
    let extract_time = start.elapsed().as_secs_f64();
    assert_eq!(e.objects.len(), 50_000);
    let start = Instant::now();
    let doc = parse(&text).unwrap();
    let parse_time = start.elapsed().as_secs_f64();
    assert_eq!(doc.objects.len(), 50_000);
    let start = Instant::now();
    let written = marlin_masset::write(&doc);
    let write_time = start.elapsed().as_secs_f64();
    assert!(written.len() > text.len() / 2);
    println!(
        "{mb:.1} MiB, 50k objects: extract {:.0} MiB/s, parse {:.0} MiB/s, write {:.0} MiB/s",
        mb / extract_time,
        mb / parse_time,
        mb / write_time
    );
}
