use marlin_shader::reflect::{Attribute, ConstValue, NamedValue, Stage};
use marlin_shaderc_ir::parse_module;
use marlin_shaderc_reflect::{Front, reflect};

const MODULE: &str = "\
struct @Game.Args { Out: res<Buffer<i32>>, Bias: f32x3, Scale: i32 }
global @Game.Main.a : ptr<push, @Game.Args>
global @Game.Tile : ptr<groupshared, [i32; 64]>

entry compute(64, 2, 1) @Game.Main uses(@Game.Main.a, @Game.Tile) {
block0:
    %0 = load @Game.Main.a
    %1 = extract.field %0, .Scale
    %2 = access @Game.Tile, [i32 0]
    store %2, %1
    return
}
";

fn front() -> Front {
    Front {
        entry: "Game.Main".into(),
        push_fields: vec![
            ("Buffer<int>".into(), vec![]),
            (
                "float3".into(),
                vec![Attribute {
                    name: "Range".into(),
                    args: vec!["0".into(), "2".into()],
                }],
            ),
            ("int".into(), vec![]),
        ],
        exports: vec![NamedValue {
            name: "Count".into(),
            ty: "uint".into(),
            value: ConstValue::UInt(2),
        }],
        capabilities: vec!["Int64".into(), "Float64".into(), "Int64".into()],
        ..Front::default()
    }
}

#[test]
fn entry_push_layout_and_stats_come_from_the_ir() {
    let m = parse_module(MODULE).unwrap();
    let entry = m.entries[0].func;
    let r = reflect(&m, entry, &m, 1234, front()).unwrap();
    assert_eq!(r.entry.name, "Game.Main");
    assert_eq!(r.entry.stage, Stage::Compute);
    assert_eq!(r.entry.group_size, [64, 2, 1]);

    let push = r.push.unwrap();
    assert_eq!(push.name, "Game.Args");
    assert_eq!(push.size, 20, "scalar layout: 4 + 12 + 4");
    let fields: Vec<_> = push
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str(), f.offset, f.size))
        .collect();
    assert_eq!(
        fields,
        [
            ("Out", "Buffer<int>", 0, 4),
            ("Bias", "float3", 4, 12),
            ("Scale", "int", 16, 4)
        ]
    );
    assert_eq!(push.fields[1].attributes[0].name, "Range");

    assert_eq!(r.capabilities, ["Float64", "Int64"], "sorted, once each");
    assert_eq!(r.exports.len(), 1);
    assert_eq!(r.stats.instructions, 5);
    assert_eq!(r.stats.groupshared_bytes, 256);
    assert_eq!(r.stats.code_bytes, 1234);
}

#[test]
fn front_and_ir_must_agree() {
    let m = parse_module(MODULE).unwrap();
    let entry = m.entries[0].func;
    let mut f = front();
    f.push_fields.pop();
    let err = reflect(&m, entry, &m, 0, f).unwrap_err();
    assert!(err.contains("3"), "{err}");

    let no_push =
        parse_module("entry compute(1, 1, 1) @Game.Empty uses() {\nblock0:\n    return\n}\n")
            .unwrap();
    let r = reflect(
        &no_push,
        no_push.entries[0].func,
        &no_push,
        0,
        Front::default(),
    )
    .unwrap();
    assert_eq!(r.push, None);
    assert_eq!(r.stats.groupshared_bytes, 0);
}
