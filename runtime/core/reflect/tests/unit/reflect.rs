use std::cell::Cell;
use std::rc::Rc;

use marlin_reflect::{
    Mut, PathErrorKind, Primitive, RangeAttr, Ref, Reflect, Registry, StructStyle, TypeId,
    TypeKind, VARIANT_FIELD, Value, get_path, set_path, type_info,
};

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Transform {
    position: [f32; 3],
    #[reflect(range = 0.1..=10.0, tooltip = "Uniform scale")]
    scale: f32,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Player {
    name: String,
    #[reflect(rename = "hp", range = 0..100)]
    health: i32,
    #[reflect(hidden, read_only)]
    id: u64,
    #[reflect(transient)]
    frame_counter: u32,
    transform: Transform,
    inventory: Vec<Item>,
    target: Option<Transform>,
    #[reflect(skip)]
    cache: NotReflected,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct NotReflected(u8);

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Item(String, u16);

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
struct Marker;

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
enum Team {
    Red = 3,
    #[reflect(default)]
    Blue = 7,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
enum Shape {
    Empty,
    Circle(#[reflect(range = 0.0..)] f32),
    Rect {
        w: f32,
        h: f32,
    },
    #[reflect(rename = "Poly")]
    Polygon {
        points: Vec<[f32; 2]>,
    },
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "test::Pair")]
struct Pair<A, B> {
    a: A,
    b: B,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Node {
    value: i32,
    children: Vec<Node>,
}

/// Uses its own `Default` rather than field-wise defaults.
#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(default, clone)]
struct Tuned {
    speed: f32,
}

impl Default for Tuned {
    fn default() -> Self {
        Self { speed: 5.0 }
    }
}

fn player() -> Player {
    Player {
        name: "ana".into(),
        health: 80,
        id: 42,
        frame_counter: 9,
        transform: Transform {
            position: [1.0, 2.0, 3.0],
            scale: 1.5,
        },
        inventory: vec![Item("sword".into(), 1), Item("potion".into(), 3)],
        target: None,
        cache: NotReflected(7),
    }
}

#[test]
fn primitive_infos() {
    let info = type_info::<u16>();
    assert!(matches!(info.kind, TypeKind::Primitive(Primitive::U16)));
    assert_eq!((info.size, info.align), (2, 2));
    assert_eq!(info.id, TypeId::of_name("u16"));
    assert!(info.is::<u16>());
    assert!(!info.is::<i16>());
    assert!(matches!(type_info::<String>().kind, TypeKind::String(_)));
}

#[test]
fn names_and_ids_of_generic_types() {
    let cases: [(&marlin_reflect::TypeInfo, &str); 5] = [
        (type_info::<Vec<f32>>(), "Vec<f32>"),
        (type_info::<Option<Vec<String>>>(), "Option<Vec<String>>"),
        (type_info::<[[u8; 4]; 12]>(), "[[u8; 4]; 12]"),
        (
            type_info::<Pair<u8, Vec<bool>>>(),
            "test::Pair<u8, Vec<bool>>",
        ),
        (type_info::<Player>(), "unit::reflect::Player"),
    ];
    for (info, name) in cases {
        assert_eq!(info.name.to_string(), name);
        // The const-computed id is the hash of the printed name.
        assert_eq!(info.id, TypeId::of_name(name), "{name}");
    }
    assert_ne!(
        type_info::<Pair<u8, u16>>().id,
        type_info::<Pair<u16, u8>>().id
    );
}

#[test]
fn struct_description() {
    let info = type_info::<Player>();
    assert_eq!(info.size, size_of::<Player>());
    assert_eq!(info.align, align_of::<Player>());
    let TypeKind::Struct(s) = &info.kind else {
        panic!("not a struct")
    };
    assert_eq!(s.style, StructStyle::Named);
    let names: Vec<_> = s.fields.iter().map(|f| f.name).collect();
    // `cache` is skipped; `health` is renamed.
    assert_eq!(
        names,
        [
            "name",
            "hp",
            "id",
            "frame_counter",
            "transform",
            "inventory",
            "target"
        ]
    );

    let hp = info.field("hp").unwrap();
    assert_eq!(hp.ident, "health");
    assert_eq!(hp.offset, std::mem::offset_of!(Player, health));
    assert_eq!(hp.name_hash, marlin_reflect::name_hash("hp"));
    assert!((hp.ty)().is::<i32>());
    assert_eq!(
        hp.attrs.range,
        Some(RangeAttr {
            min: 0.0,
            max: 100.0,
            max_inclusive: false
        })
    );
    let id = info.field("id").unwrap();
    assert!(id.attrs.hidden && id.attrs.read_only && !id.attrs.transient);
    assert!(info.field("frame_counter").unwrap().attrs.transient);

    let scale = type_info::<Transform>().field("scale").unwrap();
    assert_eq!(scale.attrs.tooltip, Some("Uniform scale"));
    let range = scale.attrs.range.unwrap();
    assert!(range.contains(10.0) && !range.contains(10.5) && !range.contains(0.0));

    let item = type_info::<Item>();
    let TypeKind::Struct(s) = &item.kind else {
        panic!()
    };
    assert_eq!(s.style, StructStyle::Tuple);
    assert_eq!(s.fields[1].name, "1");
    let TypeKind::Struct(s) = &type_info::<Marker>().kind else {
        panic!()
    };
    assert_eq!((s.style, s.fields.len()), (StructStyle::Unit, 0));
}

#[test]
fn enum_description() {
    let team = type_info::<Team>();
    let names: Vec<_> = team
        .variants()
        .iter()
        .map(|v| (v.name, v.discriminant))
        .collect();
    assert_eq!(names, [("Red", Some(3)), ("Blue", Some(7))]);
    assert_eq!(Team::reflect_default(), Some(Team::Blue));

    let shape = type_info::<Shape>();
    let variants = shape.variants();
    assert_eq!(variants.len(), 4);
    assert!(variants.iter().all(|v| v.discriminant.is_none()));
    assert_eq!(variants[1].style, StructStyle::Tuple);
    assert_eq!(
        variants[1].fields[0].attrs.range.unwrap().max,
        f64::INFINITY
    );
    assert_eq!(variants[2].fields[1].name, "h");
    assert_eq!(variants[2].fields[1].offset, VARIANT_FIELD);
    assert_eq!((variants[3].name, variants[3].ident), ("Poly", "Polygon"));
    assert_eq!(Shape::reflect_default(), Some(Shape::Empty));
}

#[test]
fn navigate_struct() {
    let p = player();
    let r = Ref::new(&p);
    assert_eq!(r.field("name").unwrap().as_str(), Some("ana"));
    assert_eq!(r.field("hp").unwrap().downcast::<i32>(), Some(&80));
    assert!(r.field("health").is_none());
    assert!(r.field("cache").is_none());
    assert_eq!(r.fields().count(), 7);

    let inv = r.field("inventory").unwrap();
    assert_eq!(inv.len(), Some(2));
    assert_eq!(
        inv.index(1).unwrap().field("0").unwrap().as_str(),
        Some("potion")
    );
    assert!(inv.index(2).is_none());
    assert_eq!(
        r.field("target").unwrap().option().map(|o| o.is_none()),
        Some(true)
    );
}

#[test]
fn paths() {
    let mut p = player();
    assert_eq!(get_path::<f32>(&p, "transform.position[2]"), Ok(&3.0));
    assert_eq!(get_path::<u16>(&p, "inventory[1].1"), Ok(&3));
    assert_eq!(
        get_path::<String>(&p, "inventory[0].0").map(String::as_str),
        Ok("sword")
    );

    set_path(&mut p, "transform.scale", 2.0f32).unwrap();
    set_path(&mut p, "inventory[0].0", String::from("axe")).unwrap();
    assert_eq!(p.transform.scale, 2.0);
    assert_eq!(p.inventory[0].0, "axe");

    let err = |path: &str| get_path::<f32>(&p, path).unwrap_err();
    assert_eq!(err("transform.nope").kind, PathErrorKind::NoField);
    assert_eq!(err("transform.nope").position, 10);
    assert_eq!(err("inventory[9]").kind, PathErrorKind::IndexOutOfBounds);
    assert_eq!(err("name[0]").kind, PathErrorKind::NotIndexable);
    assert_eq!(err("target.scale").kind, PathErrorKind::NoneOption);
    assert_eq!(err("hp").kind, PathErrorKind::TypeMismatch);
    for bad in [
        "transform..scale",
        "inventory[",
        "inventory[x]",
        ".a",
        "a.",
        "inventory[1]x",
    ] {
        assert_eq!(err(bad).kind, PathErrorKind::Syntax, "{bad}");
    }
    assert_eq!(
        set_path(&mut p, "hp", 1.0f32).unwrap_err().kind,
        PathErrorKind::TypeMismatch
    );

    // Options are looked through once they hold a value.
    p.target = Some(Transform {
        position: [0.0; 3],
        scale: 4.0,
    });
    assert_eq!(get_path::<f32>(&p, "target.scale"), Ok(&4.0));

    // The empty path is the root.
    assert!(Ref::new(&p).path("").unwrap().info().is::<Player>());
    assert_eq!(get_path::<f32>(&[1.0f32, 2.0], "[1]"), Ok(&2.0));
}

#[test]
fn navigate_enum() {
    let mut shape = Shape::Rect { w: 2.0, h: 3.0 };
    let r = Ref::new(&shape);
    assert_eq!(r.variant().unwrap().1.name, "Rect");
    assert_eq!(r.field("h").unwrap().downcast::<f32>(), Some(&3.0));
    assert!(r.field("points").is_none());

    *Mut::new(&mut shape)
        .field("w")
        .unwrap()
        .downcast::<f32>()
        .unwrap() = 5.0;
    assert_eq!(shape, Shape::Rect { w: 5.0, h: 3.0 });

    let mut m = Mut::new(&mut shape);
    assert!(m.set_variant(3));
    assert!(m.reborrow().field("points").unwrap().resize(2));
    set_path(&mut shape, "points[1][0]", 9.0f32).unwrap();
    assert_eq!(
        shape,
        Shape::Polygon {
            points: vec![[0.0; 2], [9.0, 0.0]]
        }
    );
    assert!(!Mut::new(&mut shape).set_variant(4));

    let mut circle = Shape::Circle(1.0);
    set_path(&mut circle, "0", 2.5f32).unwrap();
    assert_eq!(circle, Shape::Circle(2.5));
}

#[test]
fn mutate_through_mut() {
    let mut p = player();
    let mut m = Mut::new(&mut p);
    assert!(m.reborrow().field("name").unwrap().set_str("bo"));
    assert_eq!(m.reborrow().field("hp").unwrap().set(5i32), Ok(()));
    assert_eq!(m.reborrow().field("hp").unwrap().set(5u8), Err(5u8));

    let mut inv = m.reborrow().field("inventory").unwrap();
    assert!(inv.insert_default(0));
    assert!(inv.remove(2));
    assert!(!inv.remove(9));

    let mut target = m.reborrow().field("target").unwrap();
    assert!(target.set_some_default());
    assert!(target.reborrow().path("scale").unwrap().set(3.0f32).is_ok());

    assert_eq!(p.name, "bo");
    assert_eq!(p.health, 5);
    assert_eq!(
        p.inventory,
        [Item(String::new(), 0), Item("sword".into(), 1)]
    );
    assert_eq!(p.target.as_ref().map(|t| t.scale), Some(3.0));

    let mut m = Mut::new(&mut p);
    assert!(m.reborrow().field("target").unwrap().set_none());
    assert!(p.target.is_none());
}

#[test]
fn assign_and_reset() {
    let mut a = player();
    let b = Player {
        name: "zed".into(),
        ..player()
    };
    let mut m = Mut::new(&mut a);
    assert!(m.assign(Ref::new(&b)));
    assert!(!m.assign(Ref::new(&1u8)));
    assert_eq!(a.name, "zed");

    let mut t = Tuned { speed: 1.0 };
    assert!(Mut::new(&mut t).reset());
    assert_eq!(t.speed, 5.0);
}

#[test]
fn default_and_clone() {
    // Field-wise default: skipped fields use `Default`.
    let p = Player::reflect_default().unwrap();
    assert_eq!(p.name, "");
    assert_eq!(p.transform.position, [0.0; 3]);
    assert_eq!(p.cache, NotReflected(0));

    let original = player();
    let copy = original.reflect_clone().unwrap();
    assert_eq!(copy, original);

    // Type-level `default` and `clone` use the trait impls.
    assert_eq!(Tuned::reflect_default(), Some(Tuned { speed: 5.0 }));

    let tree = Node {
        value: 1,
        children: vec![Node {
            value: 2,
            children: vec![],
        }],
    };
    assert_eq!(tree.reflect_clone(), Some(tree.clone()));
}

/// A type without default construction or cloning, to check the `None` paths.
struct Handle(#[allow(dead_code)] u32);

// SAFETY: an opaque description of `Handle` itself.
unsafe impl Reflect for Handle {
    const TYPE_NAME: marlin_reflect::TypeName = marlin_reflect::TypeName::simple("test::Handle");
    const TYPE_INFO: marlin_reflect::TypeInfo =
        marlin_reflect::TypeInfo::new::<Self>(TypeKind::Opaque);

    fn reflect_default() -> Option<Self> {
        None
    }

    fn reflect_clone(&self) -> Option<Self> {
        None
    }
}

#[derive(Reflect)]
struct HasHandle {
    handle: Handle,
    handles: Vec<Handle>,
}

#[test]
fn missing_default_and_clone_propagate() {
    assert!(HasHandle::reflect_default().is_none());
    let value = HasHandle {
        handle: Handle(1),
        handles: vec![],
    };
    assert!(value.reflect_clone().is_none());
    assert!(Value::default_of(type_info::<HasHandle>()).is_none());

    let mut list: Vec<Handle> = vec![Handle(1)];
    let mut m = Mut::new(&mut list);
    assert!(!m.resize(3));
    assert!(m.resize(0));
    assert!(list.is_empty());
    assert!(<[Handle; 2]>::reflect_default().is_none());
}

/// Counts drops so type-erased drop and clone can be checked for leaks and double drops.
#[derive(Clone)]
struct Tracked(Rc<Cell<i32>>);

impl Drop for Tracked {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

thread_local! {
    static LIVE: Rc<Cell<i32>> = Rc::new(Cell::new(0));
}

// SAFETY: an opaque description of `Tracked` itself.
unsafe impl Reflect for Tracked {
    const TYPE_NAME: marlin_reflect::TypeName = marlin_reflect::TypeName::simple("test::Tracked");
    const TYPE_INFO: marlin_reflect::TypeInfo =
        marlin_reflect::TypeInfo::new::<Self>(TypeKind::Opaque);

    fn reflect_default() -> Option<Self> {
        let live = LIVE.with(Rc::clone);
        live.set(live.get() + 1);
        Some(Self(live))
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.0.set(self.0.get() + 1);
        Some(self.clone())
    }
}

#[derive(Reflect)]
struct Holder {
    items: Vec<Tracked>,
    one: Option<Tracked>,
    pair: [Tracked; 2],
}

#[test]
fn value_lifecycle() {
    let live = LIVE.with(Rc::clone);
    {
        let mut value = Value::default_of(type_info::<Holder>()).unwrap();
        assert_eq!(live.get(), 2);
        {
            let mut m = value.as_mut();
            assert!(m.reborrow().field("items").unwrap().resize(3));
            assert!(m.reborrow().field("one").unwrap().set_some_default());
        }
        assert_eq!(live.get(), 6);
        let copy = value.try_clone().unwrap();
        assert_eq!(live.get(), 12);
        drop(copy);
        assert_eq!(live.get(), 6);
        let holder = value.downcast::<Holder>().unwrap();
        assert_eq!(live.get(), 6);
        assert_eq!(holder.items.len(), 3);
    }
    assert_eq!(live.get(), 0);

    // Wrong-type downcasts give the value back intact.
    let value = Value::new(7u32);
    let value = value.downcast::<i32>().unwrap_err();
    assert_eq!(value.downcast::<u32>().ok(), Some(7));

    // Zero-sized values.
    let unit = Value::default_of(type_info::<Marker>()).unwrap();
    assert!(unit.try_clone().is_some());
    assert_eq!(unit.downcast::<Marker>().ok(), Some(Marker));
}

#[test]
fn registry() {
    let mut registry = Registry::new();
    registry.register::<Player>().unwrap();
    registry.register::<Player>().unwrap();
    registry.register::<Node>().unwrap();
    for name in [
        "unit::reflect::Player",
        "unit::reflect::Transform",
        "unit::reflect::Item",
        "Vec<unit::reflect::Item>",
        "Option<unit::reflect::Transform>",
        "[f32; 3]",
        "String",
        "i32",
        "unit::reflect::Node",
        "Vec<unit::reflect::Node>",
    ] {
        let info = registry
            .get_by_name(name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(registry.get(TypeId::of_name(name)).unwrap().id, info.id);
    }
    // Skipped fields are not reachable.
    assert!(
        registry
            .get_by_name("unit::reflect::NotReflected")
            .is_none()
    );
    assert!(registry.get_by_name("u128").is_none());
    assert_eq!(registry.iter().count(), registry.len());

    // A different type with the same stable name is rejected.
    #[derive(Reflect)]
    #[reflect(name = "unit::reflect::Marker")]
    struct Impostor;
    registry.register::<Marker>().unwrap();
    let err = registry.register::<Impostor>().unwrap_err();
    assert_eq!(err.id, TypeId::of_name("unit::reflect::Marker"));

    marlin_reflect::register::<Team>().unwrap();
    assert!(
        marlin_reflect::find_by_name("unit::reflect::Team")
            .unwrap()
            .is::<Team>()
    );
    assert!(marlin_reflect::find(TypeId::of_name("unit::reflect::Team")).is_some());
}

mod reexported {
    // `crate = "..."` lets the derive work through a re-export.
    pub mod engine {
        pub use marlin_reflect as reflect;
    }

    #[derive(engine::reflect::Reflect)]
    #[reflect(crate = "engine::reflect")]
    pub struct Thing {
        pub x: u8,
    }
}

#[test]
fn crate_override() {
    assert_eq!(type_info::<reexported::Thing>().fields()[0].name, "x");
}
