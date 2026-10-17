//! Binding text to reflected types and back.

use marlin_asset::{Link, SourceFile, Uuid};
use marlin_masset::{
    Attr, Document, DocumentLinks, Links, NoLinks, Node, NodeKind, Object, Schema, SchemaError,
    Severity, parse, write,
};
use marlin_reflect::{Ref, Reflect};

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
enum TextureFormat {
    #[default]
    Rgba8,
    Bc1,
    Bc7,
}

#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
struct Texture {
    format: TextureFormat,
    mip_maps: bool,
    size: [u32; 2],
    hdr_scale: f32,
    source: SourceFile,
    tags: Vec<String>,
    lod_bias: Option<f32>,
    #[reflect(transient)]
    cached: u32,
}

impl Default for Texture {
    fn default() -> Self {
        Self {
            format: TextureFormat::Bc7,
            mip_maps: true,
            size: [0, 0],
            hdr_scale: 1.0,
            source: SourceFile::default(),
            tags: Vec::new(),
            lod_bias: None,
            cached: 0,
        }
    }
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
enum Shape {
    #[default]
    Point,
    Sphere {
        radius: f32,
    },
    Box(f32, f32, f32),
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
struct Meters(f32);

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Orientation {
    rows: [[f32; 4]; 4],
    scale: Meters,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct EnvironmentProbe {
    shape: Shape,
    depth_texture: Link<Texture>,
    albedo_texture: Link<Texture>,
    general: Orientation,
    shapes: Vec<Shape>,
    big: u128,
    tiny: i8,
    letter: char,
    id: Uuid,
}

fn schema() -> Schema {
    let mut schema = Schema::new();
    schema.register::<Texture>().unwrap();
    schema.register::<EnvironmentProbe>().unwrap();
    schema
}

const TEXTURE_UUID: &str = "01832501-7918-7ee4-b177-2f200aab91b3";
const PROBE_UUID: &str = "deb8916d-3350-78d5-bf61-c2afbe805f67";
const DEPTH_UUID: &str = "a1c3916d-3350-78d5-bf61-c2afbe805f00";

fn file() -> String {
    format!(
        r#"!Version 1

Texture
{{
    !Uuid = "{TEXTURE_UUID}"
    !Name = "ProbeAlbedo"
    Format = Bc1
    MipMaps = false
    Size = (512, 256)
    HdrScale = 2
    Source = `source:level/probe_albedo.png`
    Tags = ["outdoor", "baked"]
    LodBias = -0.5
    FutureField = {{ Something = 1 }}
}}

EnvironmentProbe
{{
    !Uuid = "{PROBE_UUID}"
    !Name = "Probe"
    Shape = Sphere {{ Radius = 3.5 }}
    DepthTexture = <level/probe_depth:ProbeDepth #{DEPTH_UUID}>
    AlbedoTexture = <:ProbeAlbedo>
    General = {{ Rows = ((1 0 0 252) (0 1 0 242.08) (0 0 1 2.5) (0 0 0 1)), Scale = 4 }}
    Shapes = [Point, Box(1, 2, 3), Sphere {{ Radius = 1 }}]
    Big = "340282366920938463463374607431768211455"
    Tiny = -128
    Letter = "é"
    Id = "{TEXTURE_UUID}"
}}
"#
    )
}

/// Paths of objects outside the file, as the index would give them.
struct Index;

impl Links for Index {
    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        (target == DEPTH_UUID.parse().unwrap())
            .then(|| ("level/probe_depth".into(), "ProbeDepth".into()))
    }
}

#[test]
fn reads_every_kind_of_value() {
    let schema = schema();
    let text = file();
    let doc = parse(&text).unwrap();
    let links = DocumentLinks {
        document: &doc,
        path: "level/probe",
        fallback: &NoLinks,
    };

    let texture = schema.read_object(&doc.objects[0], &links).unwrap();
    assert_eq!(texture.uuid.to_string(), TEXTURE_UUID);
    assert_eq!(texture.name, "ProbeAlbedo");
    let warnings: Vec<_> = texture
        .diagnostics
        .iter()
        .map(|d| d.display(&text))
        .collect();
    assert_eq!(texture.diagnostics.len(), 1, "{warnings:?}");
    assert_eq!(texture.diagnostics[0].severity, Severity::Warning);
    assert!(
        warnings[0].starts_with("14:5: warning: unknown attribute `FutureField`"),
        "{warnings:?}"
    );
    assert_eq!(texture.unknown.len(), 1);
    let texture: Texture = texture.value.downcast().ok().unwrap();
    assert_eq!(
        texture,
        Texture {
            format: TextureFormat::Bc1,
            mip_maps: false,
            size: [512, 256],
            hdr_scale: 2.0,
            source: SourceFile::new("level/probe_albedo.png"),
            tags: vec!["outdoor".into(), "baked".into()],
            lod_bias: Some(-0.5),
            cached: 0,
        }
    );

    let probe = schema.read_object(&doc.objects[1], &links).unwrap();
    assert!(probe.diagnostics.is_empty(), "{:?}", probe.diagnostics);
    assert_eq!(probe.links.len(), 2);
    assert!(probe.links.iter().all(|l| l.expected_type == "Texture"));
    let probe: EnvironmentProbe = probe.value.downcast().ok().unwrap();
    assert_eq!(probe.shape, Shape::Sphere { radius: 3.5 });
    assert_eq!(probe.depth_texture.target.to_string(), DEPTH_UUID);
    // Written without a UUID; resolved from the same document.
    assert_eq!(probe.albedo_texture.target.to_string(), TEXTURE_UUID);
    assert_eq!(probe.general.rows[1], [0.0, 1.0, 0.0, 242.08]);
    assert_eq!(probe.general.scale, Meters(4.0));
    assert_eq!(
        probe.shapes,
        vec![
            Shape::Point,
            Shape::Box(1.0, 2.0, 3.0),
            Shape::Sphere { radius: 1.0 }
        ]
    );
    assert_eq!(probe.big, u128::MAX);
    assert_eq!(probe.tiny, -128);
    assert_eq!(probe.letter, 'é');
    assert_eq!(probe.id.to_string(), TEXTURE_UUID);
}

#[test]
fn writes_only_what_differs_from_defaults() {
    let schema = schema();
    let texture = Texture {
        format: TextureFormat::Bc1,
        tags: vec!["a".into()],
        lod_bias: Some(0.1),
        cached: 99,
        ..Texture::default()
    };
    let uuid: Uuid = TEXTURE_UUID.parse().unwrap();
    let object = schema
        .write_object(
            Ref::new(&texture),
            uuid,
            "Tex",
            &[],
            &NoLinks,
            "level/probe",
        )
        .unwrap();
    let text = write(&Document {
        version: 1,
        objects: vec![object],
    });
    assert_eq!(
        text,
        format!(
            "!Version 1\n\nTexture\n{{\n    !Uuid = \"{TEXTURE_UUID}\"\n    !Name = \"Tex\"\n    Format = Bc1\n    Tags = [\"a\"]\n    LodBias = 0.1\n}}\n"
        ),
        "defaults (MipMaps, Size, HdrScale, Source) and transient fields are left out; f32 0.1 stays 0.1"
    );
}

#[test]
fn round_trips_through_text_with_unknown_attributes_kept() {
    let schema = schema();
    let text = file();
    let doc = parse(&text).unwrap();
    let links = DocumentLinks {
        document: &doc,
        path: "level/probe",
        fallback: &Index,
    };
    let mut objects = Vec::new();
    let mut values = Vec::new();
    for object in &doc.objects {
        let read = schema.read_object(object, &links).unwrap();
        objects.push(
            schema
                .write_object(
                    read.value.as_ref(),
                    read.uuid,
                    &read.name,
                    &read.unknown,
                    &links,
                    "level/probe",
                )
                .unwrap(),
        );
        values.push(read.value);
    }
    let written = write(&Document {
        version: 1,
        objects,
    });
    assert!(
        written.contains("FutureField = { Something = 1 }"),
        "{written}"
    );
    // The hand-written link got its UUID; the same-file link stays local.
    assert!(
        written.contains(&format!("AlbedoTexture = <:ProbeAlbedo #{TEXTURE_UUID}>")),
        "{written}"
    );
    assert!(written.contains(&format!(
        "DepthTexture = <level/probe_depth:ProbeDepth #{DEPTH_UUID}>"
    )));
    assert!(written.contains("Big = \"340282366920938463463374607431768211455\""));

    // Reading the written text gives the same values, and writing again is a fixed point.
    let doc2 = parse(&written).unwrap();
    let links2 = DocumentLinks {
        document: &doc2,
        path: "level/probe",
        fallback: &Index,
    };
    let texture: Texture = schema
        .read_object(&doc2.objects[0], &links2)
        .unwrap()
        .value
        .downcast()
        .ok()
        .unwrap();
    assert_eq!(Some(&texture), values[0].as_ref().downcast::<Texture>());
    let probe: EnvironmentProbe = schema
        .read_object(&doc2.objects[1], &links2)
        .unwrap()
        .value
        .downcast()
        .ok()
        .unwrap();
    assert_eq!(
        Some(&probe),
        values[1].as_ref().downcast::<EnvironmentProbe>()
    );
    let mut objects = Vec::new();
    for object in &doc2.objects {
        let read = schema.read_object(object, &links2).unwrap();
        objects.push(
            schema
                .write_object(
                    read.value.as_ref(),
                    read.uuid,
                    &read.name,
                    &read.unknown,
                    &links2,
                    "level/probe",
                )
                .unwrap(),
        );
    }
    assert_eq!(
        write(&Document {
            version: 1,
            objects
        }),
        written
    );
}

#[test]
fn bad_values_are_reported_and_reading_continues() {
    let schema = schema();
    let text = format!(
        r#"!Version 1
Texture
{{
    !Uuid = "{TEXTURE_UUID}"
    !Name = "T"
    Format = Bc9
    MipMaps = 1
    Size = (1, 2, 3)
    HdrScale = "big"
    Tags = "solo"
    LodBias = 0.25
}}
EnvironmentProbe
{{
    !Uuid = "{PROBE_UUID}"
    !Name = "P"
    Tiny = 200
    Shape = Box(1)
    DepthTexture = <elsewhere:Missing>
    Letter = "ab"
}}
"#
    );
    let doc = parse(&text).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    let messages: Vec<String> = read.diagnostics.iter().map(|d| d.display(&text)).collect();
    assert_eq!(messages.len(), 5, "{messages:#?}");
    assert!(
        messages[0]
            .starts_with("6:14: error: unknown variant `Bc9`; expected one of Rgba8, Bc1, Bc7")
    );
    assert!(messages[1].contains("expected `true` or `false`, found an integer"));
    assert!(messages[2].contains("expected a tuple of 2"));
    assert!(messages[3].contains("expected a number, found a string"));
    assert!(messages[4].contains("expected a list"));
    // Good fields still arrive; bad ones keep their defaults.
    let texture: Texture = read.value.downcast().ok().unwrap();
    assert_eq!(texture.lod_bias, Some(0.25));
    assert_eq!(texture.format, TextureFormat::Bc7);

    let read = schema.read_object(&doc.objects[1], &NoLinks).unwrap();
    let messages: Vec<String> = read.diagnostics.iter().map(|d| d.message.clone()).collect();
    assert_eq!(messages.len(), 4, "{messages:#?}");
    assert!(messages[0].contains("out of range for i8"));
    assert!(messages[1].contains("`Box(…)` with 3 values"));
    assert!(messages[2].contains("has no UUID and does not resolve"));
    assert!(messages[3].contains("one-character string"));
}

#[test]
fn unknown_types_and_newer_versions_fail_the_object() {
    let schema = schema();
    let doc = parse(&format!(
        "!Version 1\nMystery {{ !Uuid = \"{TEXTURE_UUID}\" !Name = \"M\" }}\nTexture {{ !Uuid = \"{PROBE_UUID}\" !Name = \"T\" !Version = 3 }}"
    ))
    .unwrap();
    let e = schema.read_object(&doc.objects[0], &NoLinks).err().unwrap();
    assert!(e.message.contains("unknown object type `Mystery`"));
    let e = schema.read_object(&doc.objects[1], &NoLinks).err().unwrap();
    assert!(e.message.contains("newer than this tool's 0"));
}

mod v2 {
    use marlin_reflect::Reflect;

    /// Version 2 renamed `Radius` to `Size` and doubled it (version 0 -> 1), then added `Kind`
    /// (1 -> 2).
    #[derive(Clone, PartialEq, Debug, Default, Reflect)]
    pub struct Light {
        pub size: f32,
        pub kind: u32,
    }
}

#[test]
fn schema_upgrades_run_in_order() {
    let mut schema = Schema::new();
    schema.register_versioned::<v2::Light>(2).unwrap();
    assert!(schema.register_upgrade("Light", 0, |object: &mut Object| {
        let attr = object
            .attrs
            .iter_mut()
            .find(|a| a.key == "Radius")
            .ok_or("no Radius")?;
        attr.key = "Size".into();
        if let NodeKind::Float(v) = attr.value.kind {
            attr.value = Node::new(NodeKind::Float(v * 2.0));
        }
        Ok(())
    }));
    assert!(schema.register_upgrade("Light", 1, |object: &mut Object| {
        object.attrs.push(Attr {
            key: "Kind".into(),
            value: Node::new(NodeKind::Int(7)),
            span: Default::default(),
        });
        Ok(())
    }));
    assert!(
        !schema.register_upgrade("Light", 2, |_| Ok(())),
        "no step from the current version"
    );

    let doc = parse(&format!(
        "!Version 1\nLight {{ !Uuid = \"{PROBE_UUID}\" !Name = \"L\" Radius = 1.5 }}"
    ))
    .unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let light: v2::Light = read.value.downcast().ok().unwrap();
    assert_eq!(light, v2::Light { size: 3.0, kind: 7 });

    let object = schema
        .write_object(Ref::new(&light), read.uuid, "L", &[], &NoLinks, "")
        .unwrap();
    assert_eq!(object.type_version, 2, "written at the current version");

    let doc = parse(&format!(
        "!Version 1\nLight {{ !Uuid = \"{PROBE_UUID}\" !Name = \"L\" Size = 1.5 }}"
    ))
    .unwrap();
    let e = schema.read_object(&doc.objects[0], &NoLinks).err().unwrap();
    assert!(
        e.message
            .contains("upgrading `Light` from version 0: no Radius"),
        "{}",
        e.message
    );
}

mod a {
    #[derive(Default, marlin_reflect::Reflect)]
    pub struct Same {
        pub x: u32,
    }
}

mod b {
    #[derive(Default, marlin_reflect::Reflect)]
    pub struct Same {
        pub y: u32,
    }
}

#[derive(Default, Reflect)]
struct Ambiguous {
    mip_0_size: u32,
    mip0_size: u32,
}

#[test]
fn schema_rejects_ambiguous_types() {
    let mut schema = Schema::new();
    schema.register::<a::Same>().unwrap();
    assert!(matches!(
        schema.register::<b::Same>(),
        Err(SchemaError::DuplicateType { name, .. }) if name == "Same"
    ));
    assert!(matches!(
        schema.register::<Ambiguous>(),
        Err(SchemaError::FieldCollision { pascal, .. }) if pascal == "Mip0Size"
    ));
    assert!(matches!(
        schema.register::<Link<Texture>>(),
        Err(SchemaError::Generic(_))
    ));
    assert!(matches!(
        schema.register::<Shape>(),
        Err(SchemaError::NotStruct(_))
    ));
    assert!(
        schema.register::<a::Same>().is_ok(),
        "re-registering is fine"
    );
}

#[test]
fn nil_links_are_none_and_unknown_targets_are_marked() {
    let schema = schema();
    let probe = EnvironmentProbe {
        depth_texture: Link::new(Uuid::from_u128(5)),
        ..EnvironmentProbe::default()
    };
    let object = schema
        .write_object(
            Ref::new(&probe),
            PROBE_UUID.parse().unwrap(),
            "P",
            &[],
            &NoLinks,
            "x",
        )
        .unwrap();
    let text = marlin_masset::inline(&object.attrs[0].value);
    assert_eq!(object.attrs[0].key, "DepthTexture");
    assert_eq!(text, "<?:? #00000000-0000-0000-0000-000000000005>");
    assert!(
        object.attrs.iter().all(|a| a.key != "AlbedoTexture"),
        "nil link is the default"
    );
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Layer {
    opacity: f32,
    name: String,
}

#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
struct Stack {
    layers: Vec<Layer>,
}

impl Default for Stack {
    fn default() -> Self {
        let layer = Layer {
            opacity: 0.5,
            name: "base".into(),
        };
        Self {
            layers: vec![layer.clone(), layer],
        }
    }
}

#[test]
fn list_items_reset_to_the_element_default_round_trip() {
    let mut schema = Schema::new();
    schema.register::<Stack>().unwrap();
    // Item 0 is back at the element default (which the writer leaves out entirely), item 1
    // equals the enclosing default's item, item 2 is past its end.
    let stack = Stack {
        layers: vec![
            Layer::default(),
            Layer {
                opacity: 0.5,
                name: "base".into(),
            },
            Layer {
                opacity: 0.0,
                name: "top".into(),
            },
        ],
    };
    let object = schema
        .write_object(
            Ref::new(&stack),
            PROBE_UUID.parse().unwrap(),
            "S",
            &[],
            &NoLinks,
            "",
        )
        .unwrap();
    let text = write(&Document {
        version: 1,
        objects: vec![object],
    });
    let doc = parse(&text).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    assert_eq!(read.value.downcast::<Stack>().ok(), Some(stack), "{text}");
}
