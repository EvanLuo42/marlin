//! Parsing and canonical writing of untyped documents.

use marlin_asset::Uuid;
use marlin_masset::{
    Document, FileRef, LinkRef, Node, NodeKind, case, inline, parse, parse_value, write,
};
use marlin_math::Rng;

const PROBE: &str = r#"!Version 1

// A light probe and its textures.
EnvironmentProbe
{
    !Uuid = "deb8916d-3350-78d5-bf61-c2afbe805f67"
    !Name = "Probe1"
    Kind = DynamicLighting
    DepthTexture = <level/probe1_depth:Probe1Depth #a1c3916d-3350-78d5-bf61-c2afbe805f00>
    AlbedoTexture = <:Probe1Albedo #01832501-7918-7ee4-b177-2f200aab91b3>
    Position = (252, 242.08, 2.5)
    Tags = ["outdoor" "baked"]   /* commas are optional */
    General = { Orientation = ((1 0 0 0) (0 1 0 0) (0 0 1 0) (0 0 0 1)) }
}

Texture
{
    !Uuid = "01832501-7918-7ee4-b177-2f200aab91b3"
    !Name = "Probe1Albedo"
    !Version = 2
    Format = Bc1
    MipMaps = false
    Shape = Box(1.5, -2, 3e2)
    Fancy = Sphere { Radius = 0.25 }
    Source = `source:level/probe1_albedo.png`
    Comment = None
}
"#;

fn node(kind: NodeKind) -> Node {
    Node::new(kind)
}

#[test]
fn parses_every_construct() {
    let doc = parse(PROBE).unwrap();
    assert_eq!(doc.version, 1);
    assert_eq!(doc.objects.len(), 2);

    let probe = &doc.objects[0];
    assert_eq!(probe.type_name, "EnvironmentProbe");
    assert_eq!(probe.name, "Probe1");
    assert_eq!(
        probe.uuid.to_string(),
        "deb8916d-3350-78d5-bf61-c2afbe805f67"
    );
    assert_eq!(probe.type_version, 0);
    let attr = |name: &str| {
        &probe
            .attrs
            .iter()
            .find(|a| a.key == name)
            .unwrap()
            .value
            .kind
    };
    assert_eq!(*attr("Kind"), NodeKind::Ident("DynamicLighting".into()));
    assert_eq!(
        *attr("DepthTexture"),
        NodeKind::Link(LinkRef {
            path: "level/probe1_depth".into(),
            name: "Probe1Depth".into(),
            uuid: Some("a1c3916d-3350-78d5-bf61-c2afbe805f00".parse().unwrap()),
        })
    );
    let NodeKind::Link(local) = attr("AlbedoTexture") else {
        panic!()
    };
    assert!(local.is_local());
    assert_eq!(
        *attr("Position"),
        NodeKind::Tuple(vec![
            node(NodeKind::Int(252)),
            node(NodeKind::Float(242.08)),
            node(NodeKind::Float(2.5)),
        ])
    );
    assert_eq!(
        *attr("Tags"),
        NodeKind::List(vec![
            node(NodeKind::Str("outdoor".into())),
            node(NodeKind::Str("baked".into())),
        ])
    );
    let NodeKind::Record(None, general) = attr("General") else {
        panic!()
    };
    let NodeKind::Tuple(rows) = &general[0].value.kind else {
        panic!()
    };
    assert_eq!(rows.len(), 4);

    let texture = &doc.objects[1];
    assert_eq!(texture.type_version, 2);
    let attr = |name: &str| {
        &texture
            .attrs
            .iter()
            .find(|a| a.key == name)
            .unwrap()
            .value
            .kind
    };
    assert_eq!(*attr("MipMaps"), NodeKind::Bool(false));
    assert_eq!(
        *attr("Shape"),
        NodeKind::Call(
            "Box".into(),
            vec![
                node(NodeKind::Float(1.5)),
                node(NodeKind::Int(-2)),
                node(NodeKind::Float(300.0)),
            ]
        )
    );
    let NodeKind::Record(Some(tag), fields) = attr("Fancy") else {
        panic!()
    };
    assert_eq!(tag, "Sphere");
    assert_eq!(fields[0].value.kind, NodeKind::Float(0.25));
    assert_eq!(
        *attr("Source"),
        NodeKind::File(FileRef {
            scheme: "source".into(),
            path: "level/probe1_albedo.png".into(),
        })
    );
    assert_eq!(*attr("Comment"), NodeKind::Ident("None".into()));
}

#[test]
fn writing_is_canonical_and_round_trips() {
    let doc = parse(PROBE).unwrap();
    let text = write(&doc);
    let again = parse(&text).unwrap();
    assert_eq!(again, doc, "write then parse gives the same tree");
    assert_eq!(
        write(&again),
        text,
        "canonical: a second write is identical"
    );
    assert!(!text.contains("//"), "comments are not kept");
    assert!(text.starts_with("!Version 1\n\nEnvironmentProbe\n{\n    !Uuid = "));
    assert!(text.contains("    Tags = [\"outdoor\", \"baked\"]\n"));
    assert!(text.contains("    Position = (252, 242.08, 2.5)\n"));
    assert!(text.contains("    !Version = 2\n"));
    assert!(text.contains("    Shape = Box(1.5, -2, 300.0)\n"));
    assert!(text.contains("    Fancy = Sphere { Radius = 0.25 }\n"));
    assert!(
        text.contains(
            "    AlbedoTexture = <:Probe1Albedo #01832501-7918-7ee4-b177-2f200aab91b3>\n"
        )
    );
}

#[test]
fn long_values_break_one_element_per_line() {
    let long: Vec<String> = (0..20).map(|i| format!("\"element number {i}\"")).collect();
    let text = format!(
        "!Version 1\nThing {{ !Uuid = \"{}\" !Name = \"A\" Items = [{}] Nested = {{ A = 1 B = Some {{ C = [{}] }} }} }}",
        Uuid::from_u128(7),
        long.join(" "),
        long.join(" "),
    );
    let doc = parse(&text).unwrap();
    let out = write(&doc);
    assert!(out.contains("    Items =\n    [\n        \"element number 0\"\n"));
    assert!(out.contains("    Nested =\n    {\n        A = 1\n        B = Some\n        {\n            C =\n            [\n"));
    for line in out.lines() {
        assert!(line.len() <= marlin_masset::MAX_WIDTH, "{line}");
    }
    let again = parse(&out).unwrap();
    assert_eq!(again, doc);
    assert_eq!(write(&again), out);
}

#[test]
fn scalars_round_trip_exactly() {
    for text in [
        "0",
        "-1",
        "170141183460469231731687303715884105727",
        "0.1",
        "1.0",
        "-0.0",
        "1e-7",
        "1.7976931348623157e308",
        "inf",
        "-inf",
        "\"quote \\\" backslash \\\\ newline \\n tab \\t unicode é \\u{1}\"",
        "true",
        "None",
        "`source:dir/file with.png`",
        "<a/b:C #00000000-0000-0000-0000-000000000001>",
        "<a/b:C>",
        "[]",
        "{}",
        "()",
    ] {
        let node = parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        let written = inline(&node);
        let again = parse_value(&written).unwrap();
        assert_eq!(again, node, "{text} -> {written}");
    }
    assert_eq!(inline(&parse_value("0x_ff").unwrap()), "255");
    assert_eq!(inline(&parse_value("1_000").unwrap()), "1000");
    assert_eq!(inline(&parse_value("2.50").unwrap()), "2.5");
    assert!(matches!(parse_value("nan").unwrap().kind, NodeKind::Float(v) if v.is_nan()));
}

#[test]
fn errors_point_at_the_problem() {
    let cases: &[(&str, &str, u32, u32)] = &[
        ("Thing {}", "`!Version`", 1, 1),
        (
            "!Version 1\nThing\n{\n    A = \n}",
            "expected a value",
            5,
            1,
        ),
        ("!Version 1\nThing { !Name = \"A\" }", "no `!Uuid`", 2, 1),
        (
            "!Version 1\nThing { !Uuid = \"00000000-0000-0000-0000-000000000001\" }",
            "no `!Name`",
            2,
            1,
        ),
        (
            "!Version 1\nThing { !Uuid = \"nope\" !Name = \"A\" }",
            "malformed UUID",
            2,
            17,
        ),
        (
            "!Version 1\nThing { A = \"open\n}",
            "unterminated string",
            2,
            13,
        ),
        ("!Version 1\nThing { A = 12px }", "malformed number", 2, 13),
        (
            "!Version 1\nThing { A = <broken> }",
            "<path:Name #uuid>",
            2,
            13,
        ),
        ("!Version 1\nThing { A = 1 A = 2 }", "given twice", 2, 15),
        ("!Version 1\nThing { A = (1 2 }", "expected `)`", 2, 18),
        (
            "!Version 1\nThing { A = 1 } $",
            "unexpected character",
            2,
            17,
        ),
        (
            "!Version 1\nThing { !Bogus = 1 }",
            "unknown directive",
            2,
            9,
        ),
    ];
    for &(text, message, line, column) in cases {
        let error = parse(text).expect_err(text);
        assert!(error.message.contains(message), "{text}: {error}");
        assert_eq!(
            (error.location.line, error.location.column),
            (line, column),
            "{text}: {error}"
        );
    }
}

#[test]
fn duplicate_names_and_uuids_are_rejected() {
    let a = "Thing { !Uuid = \"00000000-0000-0000-0000-000000000001\" !Name = \"A\" }";
    let b = "Thing { !Uuid = \"00000000-0000-0000-0000-000000000002\" !Name = \"A\" }";
    let c = "Thing { !Uuid = \"00000000-0000-0000-0000-000000000001\" !Name = \"C\" }";
    assert!(
        parse(&format!("!Version 1\n{a}\n{b}"))
            .unwrap_err()
            .message
            .contains("named `A`")
    );
    assert!(
        parse(&format!("!Version 1\n{a}\n{c}"))
            .unwrap_err()
            .message
            .contains("UUID")
    );
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let text = format!(
        "!Version 1\nThing {{ !Uuid = \"00000000-0000-0000-0000-000000000001\" !Name = \"A\" X = {}1{} }}",
        "[".repeat(10_000),
        "]".repeat(10_000)
    );
    assert!(parse(&text).unwrap_err().message.contains("deeply"));
}

#[test]
fn garbage_never_panics() {
    // Deterministic xorshift so failures reproduce.
    let mut rng = Rng::new(0x2545_f491_4f6c_dd1d);
    let mut next = || rng.next_u64();
    let base = PROBE.as_bytes();
    for _ in 0..20_000 {
        let mut bytes = base.to_vec();
        for _ in 0..(next() % 8 + 1) {
            let i = (next() % bytes.len() as u64) as usize;
            match next() % 3 {
                0 => bytes[i] = next() as u8,
                1 => {
                    bytes.remove(i);
                }
                _ => bytes.truncate(i),
            }
            if bytes.is_empty() {
                break;
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        let _ = parse(&text);
        let _ = marlin_masset::extract(&text);
    }
}

#[test]
fn byte_order_mark_and_crlf_are_accepted() {
    let text = format!("\u{feff}{}", PROBE.replace('\n', "\r\n"));
    assert_eq!(parse(&text).unwrap(), parse(PROBE).unwrap());
}

#[test]
fn empty_document_writes_header_only() {
    let doc = Document {
        version: marlin_masset::FORMAT_VERSION,
        objects: Vec::new(),
    };
    assert_eq!(write(&doc), "!Version 1\n");
}

#[test]
fn rust_names_convert_to_pascal_case() {
    for (rust, text) in [
        ("hdr_texture", "HdrTexture"),
        ("mip_0_size", "Mip0Size"),
        ("x", "X"),
        ("depth_texture", "DepthTexture"),
        ("Bc7", "Bc7"),
        ("DynamicLighting", "DynamicLighting"),
        ("_private", "Private"),
    ] {
        assert_eq!(case::to_pascal(rust), text);
    }
    assert_eq!(case::short_type_name("marlin_render::Camera"), "Camera");
}
