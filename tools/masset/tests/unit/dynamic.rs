//! Dynamic holders in text: `Type { … }` values of types chosen at run time, and proxies.

use marlin_masset::Schema;
use marlin_masset::{Document, NoLinks, Severity, parse, write};
use marlin_reflect::{
    DynamicHolder, Proxied, Ref, Reflect, ReflectDynamic, ReflectProxy, TypeInfo, TypeKind,
    TypeName, Value, find_data,
};

struct Allowed;

#[derive(Default)]
struct Holder(Option<Value>);

impl DynamicHolder for Holder {
    fn held(&self) -> Option<Ref<'_>> {
        self.0.as_ref().map(Value::as_ref)
    }

    fn hold(&mut self, value: Value) -> Result<(), Value> {
        if Self::accepts(value.info()) {
            self.0 = Some(value);
            Ok(())
        } else {
            Err(value)
        }
    }

    fn accepts(info: &'static TypeInfo) -> bool {
        find_data::<Allowed>(info).is_some()
    }
}

// SAFETY: an opaque description of `Holder` itself.
unsafe impl Reflect for Holder {
    const TYPE_NAME: TypeName = TypeName::simple("masset_test::Holder");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectDynamic::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Self(None))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Self(match &self.0 {
            Some(v) => Some(v.try_clone()?),
            None => None,
        }))
    }
}

/// A 24-bit color, written as its hex string through a proxy.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Rgb(u32);

impl Proxied for Rgb {
    type Proxy = String;

    fn to_proxy(&self) -> String {
        format!("#{:06x}", self.0)
    }

    fn set_from_proxy(&mut self, proxy: &String) -> bool {
        match proxy
            .strip_prefix('#')
            .and_then(|h| u32::from_str_radix(h, 16).ok())
        {
            Some(v) if v <= 0xff_ffff => {
                self.0 = v;
                true
            }
            _ => false,
        }
    }
}

// SAFETY: an opaque description of `Rgb` itself.
unsafe impl Reflect for Rgb {
    const TYPE_NAME: TypeName = TypeName::simple("masset_test::Rgb");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectProxy::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Self(0))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(name = "masset_test::Light", default)]
struct Light {
    intensity: f32,
    color: Rgb,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            intensity: 1.0,
            color: Rgb(0xff_ffff),
        }
    }
}

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(name = "masset_test::Tag")]
struct Tag;

#[derive(Reflect, Default)]
#[reflect(name = "masset_test::Bag", default)]
struct Bag {
    items: Vec<Holder>,
}

fn schema() -> Schema {
    for info in [
        marlin_reflect::type_info::<Light>(),
        marlin_reflect::type_info::<Tag>(),
    ] {
        marlin_reflect::add_data(info, Allowed).unwrap();
    }
    let mut schema = Schema::new();
    schema.register::<Bag>().unwrap();
    schema.register::<Light>().unwrap();
    schema.register::<Tag>().unwrap();
    schema
}

const TEXT: &str = r##"!Version 1

Bag
{
    !Uuid = "01992d00-0000-7000-8000-0000000000aa"
    !Name = "Things"
    Items = [Light { Intensity = 800.0, Color = "#ff8000" }, Tag, None, Light]
}
"##;

#[test]
fn held_values_read_and_write_with_their_type_names() {
    let schema = schema();
    let doc = parse(TEXT).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let bag = read.value.as_ref().downcast::<Bag>().unwrap();
    let held = |i: usize| bag.items[i].held();
    assert_eq!(
        held(0).and_then(|r| r.downcast::<Light>()),
        Some(&Light {
            intensity: 800.0,
            color: Rgb(0xff8000)
        })
    );
    assert_eq!(held(1).and_then(|r| r.downcast::<Tag>()), Some(&Tag));
    assert!(held(2).is_none());
    assert_eq!(
        held(3).and_then(|r| r.downcast::<Light>()),
        Some(&Light::default())
    );

    let object = schema
        .write_object(
            read.value.as_ref(),
            read.uuid,
            &read.name,
            &[],
            &NoLinks,
            "",
        )
        .unwrap();
    let written = write(&Document {
        version: 1,
        objects: vec![object],
    });
    assert_eq!(written, TEXT, "canonical text is a fixed point");
}

#[test]
fn unknown_and_refused_types_are_reported() {
    let schema = schema();
    let text = TEXT.replace("Items = [", "Items = [Nowhere { A = 1 }, Holder, 5, ");
    let doc = parse(&text).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    let errors: Vec<&str> = read
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors[0].contains("`Nowhere` is not a type"), "{errors:?}");
    assert!(errors[1].contains("`Holder` is not a type"), "{errors:?}");
    let bag = read.value.as_ref().downcast::<Bag>().unwrap();
    assert_eq!(bag.items.len(), 7, "bad items stay as empty holders");
}

#[test]
fn proxies_reject_invalid_text() {
    let schema = schema();
    let text = TEXT.replace("\"#ff8000\"", "\"#zz\"");
    let doc = parse(&text).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    assert_eq!(read.diagnostics.len(), 1, "{:?}", read.diagnostics);
    assert!(read.diagnostics[0].message.contains("not a valid"));
}

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(name = "masset_test::Pose", default)]
struct Pose {
    position: marlin_math::Vec3,
    rotation: marlin_math::Quat,
}

#[test]
fn vectors_and_quaternions_are_tuples() {
    let mut schema = Schema::new();
    schema.register::<Pose>().unwrap();
    let text = r#"!Version 1

Pose
{
    !Uuid = "01992d00-0000-7000-8000-0000000000bb"
    !Name = "P"
    Position = (1.0, 2.5, -3.0)
    Rotation = (0.0, 0.6, 0.0, 0.8)
}
"#;
    let doc = parse(text).unwrap();
    let read = schema.read_object(&doc.objects[0], &NoLinks).unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let pose = read.value.as_ref().downcast::<Pose>().unwrap();
    assert_eq!(pose.position, marlin_math::Vec3::new(1.0, 2.5, -3.0));
    assert_eq!(pose.rotation.y, 0.6);
    let object = schema
        .write_object(
            read.value.as_ref(),
            read.uuid,
            &read.name,
            &[],
            &NoLinks,
            "",
        )
        .unwrap();
    let written = write(&Document {
        version: 1,
        objects: vec![object],
    });
    assert_eq!(written, text);

    // Named fields read too (hand-written text); writing gives the tuple.
    let named = text.replace("(1.0, 2.5, -3.0)", "{ X = 1.0, Z = -3.0 }");
    let read = schema
        .read_object(&parse(&named).unwrap().objects[0], &NoLinks)
        .unwrap();
    assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
    let pose = read.value.as_ref().downcast::<Pose>().unwrap();
    assert_eq!(pose.position, marlin_math::Vec3::new(1.0, 0.0, -3.0));
    let short = text.replace("(1.0, 2.5, -3.0)", "(1.0, 2.5)");
    let read = schema
        .read_object(&parse(&short).unwrap().objects[0], &NoLinks)
        .unwrap();
    assert_eq!(read.diagnostics.len(), 1, "a tuple needs every field");
}
