//! Tuples, maps, sets, pointers, the engine types, type data and the registry's run-time data.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use marlin_containers::{ArrayVec, Handle, HashMap, HashMapExt, HashSet, HashSetExt, SmallVec};
use marlin_math::{Mat4, Quat, Transform, Vec3, Vec4};
use marlin_reflect::{
    IdCollision, Mut, PathErrorKind, PointerKind, Proxied, Ref, Reflect, ReflectProxy,
    ReflectTrait, Registry, TypeId, TypeKind, Value, get_path, reflect_trait, set_path, type_info,
};
use marlin_text::{InlineString, Name};

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Scene {
    pair: (u8, String),
    names: BTreeMap<String, u32>,
    tags: BTreeSet<u16>,
    lookup: HashMap<u32, Vec<f32>>,
    seen: HashSet<String>,
    boxed: Box<Inner>,
    shared: Rc<Inner>,
    queue: VecDeque<i8>,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Inner {
    value: i32,
}

fn scene() -> Scene {
    let mut lookup = <HashMap<u32, Vec<f32>> as HashMapExt>::new();
    lookup.insert(7, vec![1.0, 2.0]);
    let mut seen = <HashSet<String> as HashSetExt>::new();
    seen.insert("a".into());
    Scene {
        pair: (3, "three".into()),
        names: BTreeMap::from([("b".into(), 2), ("a".into(), 1)]),
        tags: BTreeSet::from([5, 1]),
        lookup,
        seen,
        boxed: Box::new(Inner { value: 10 }),
        shared: Rc::new(Inner { value: 20 }),
        queue: VecDeque::from([1, 2]),
    }
}

#[test]
fn tuple_names_and_fields() {
    let cases: [(&marlin_reflect::TypeInfo, &str); 3] = [
        (type_info::<(u8,)>(), "(u8,)"),
        (type_info::<(u8, String)>(), "(u8, String)"),
        (
            type_info::<(u8, (bool, f32), Vec<(i8, i8)>)>(),
            "(u8, (bool, f32), Vec<(i8, i8)>)",
        ),
    ];
    for (info, name) in cases {
        assert_eq!(info.name.to_string(), name);
        assert_eq!(info.id, TypeId::of_name(name), "{name}");
    }
    let info = type_info::<(u8, u64, u16)>();
    let TypeKind::Tuple(t) = &info.kind else {
        panic!("not a tuple")
    };
    assert_eq!(t.fields.len(), 3);
    assert_eq!(t.fields[1].name, "1");
    assert_eq!(t.fields[1].offset, std::mem::offset_of!((u8, u64, u16), 1));
    assert_eq!(info.fields().len(), 3);

    let mut s = scene();
    assert_eq!(get_path::<u8>(&s, "pair.0"), Ok(&3));
    set_path(&mut s, "pair.1", String::from("four")).unwrap();
    assert_eq!(s.pair.1, "four");
    assert_eq!(Ref::new(&s.pair).fields().count(), 2);
    let twelve = (0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8, 9u8, 10u8, 11u8);
    assert_eq!(get_path::<u8>(&twelve, "11"), Ok(&11));
    assert_eq!(<(u8, String)>::reflect_default(), Some((0, String::new())));
}

#[test]
fn maps() {
    let mut s = scene();
    let names = Ref::new(&s).field("names").unwrap();
    assert_eq!(names.len(), Some(2));
    let TypeKind::Map(m) = &names.info().kind else {
        panic!()
    };
    assert!(m.ordered);
    let keys: Vec<_> = names
        .map_entries()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().unwrap().to_owned(),
                *v.downcast::<u32>().unwrap(),
            )
        })
        .collect();
    assert_eq!(keys, [("a".to_owned(), 1), ("b".to_owned(), 2)]);
    let key = String::from("b");
    assert_eq!(
        names.map_get(Ref::new(&key)).unwrap().downcast::<u32>(),
        Some(&2)
    );
    // A key of the wrong type finds nothing.
    assert!(names.map_get(Ref::new(&1u8)).is_none());

    let mut m = Mut::new(&mut s);
    let mut names = m.reborrow().field("names").unwrap();
    assert!(
        names
            .map_insert(Value::new(String::from("c")), Value::new(3u32))
            .is_ok()
    );
    // Wrong value type: both are handed back.
    let (k, v) = names
        .map_insert(Value::new(String::from("d")), Value::new(4u8))
        .unwrap_err();
    assert!(k.info().is::<String>() && v.info().is::<u8>());
    *names
        .reborrow()
        .map_get_mut(Ref::new(&key))
        .unwrap()
        .downcast::<u32>()
        .unwrap() = 20;
    assert!(names.map_remove(Ref::new(&String::from("a"))));
    assert!(!names.map_remove(Ref::new(&String::from("a"))));
    for (_, mut v) in names.reborrow().map_entries_mut().unwrap() {
        *v.reborrow().downcast::<u32>().unwrap() += 1;
    }
    assert_eq!(s.names, BTreeMap::from([("b".into(), 21), ("c".into(), 4)]));

    let lookup = Ref::new(&s).field("lookup").unwrap();
    let TypeKind::Map(m) = &lookup.info().kind else {
        panic!()
    };
    assert!(!m.ordered);
    assert_eq!(get_path::<f32>(&s.lookup[&7], "[1]"), Ok(&2.0));
    let mut m = Mut::new(&mut s);
    let mut lookup = m.reborrow().field("lookup").unwrap();
    assert!(lookup.clear());
    assert!(s.lookup.is_empty());
}

#[test]
fn sets() {
    let mut s = scene();
    let tags = Ref::new(&s).field("tags").unwrap();
    let elements: Vec<u16> = tags
        .set_elements()
        .unwrap()
        .iter()
        .map(|e| *e.downcast::<u16>().unwrap())
        .collect();
    assert_eq!(elements, [1, 5]);
    assert_eq!(tags.set_contains(Ref::new(&5u16)), Some(true));
    assert_eq!(tags.set_contains(Ref::new(&5u32)), None);

    let mut m = Mut::new(&mut s);
    let mut tags = m.reborrow().field("tags").unwrap();
    assert_eq!(tags.set_insert(Value::new(9u16)).ok(), Some(true));
    assert_eq!(tags.set_insert(Value::new(9u16)).ok(), Some(false));
    assert!(tags.set_insert(Value::new(9u8)).is_err());
    assert!(tags.set_remove(Ref::new(&1u16)));
    let mut seen = m.reborrow().field("seen").unwrap();
    assert_eq!(
        seen.set_insert(Value::new(String::from("b"))).ok(),
        Some(true)
    );
    assert_eq!(s.tags, BTreeSet::from([5, 9]));
    assert_eq!(s.seen.len(), 2);
}

#[test]
fn pointers() {
    let mut s = scene();
    assert_eq!(get_path::<i32>(&s, "boxed.value"), Ok(&10));
    assert_eq!(get_path::<i32>(&s, "shared.value"), Ok(&20));
    set_path(&mut s, "boxed.value", 11).unwrap();
    // A unique `Rc` can be written through.
    set_path(&mut s, "shared.value", 21).unwrap();
    assert_eq!((s.boxed.value, s.shared.value), (11, 21));

    let info = type_info::<Rc<Inner>>();
    let TypeKind::Pointer(p) = &info.kind else {
        panic!()
    };
    assert_eq!(p.kind, PointerKind::Rc);
    assert_eq!(info.name.to_string(), "Rc<unit::kinds::Inner>");

    // A shared one cannot, through paths or `pointee`.
    let other = Rc::clone(&s.shared);
    assert_eq!(
        set_path(&mut s, "shared.value", 22).unwrap_err().kind,
        PathErrorKind::SharedPointer
    );
    let shared = Mut::new(&mut s).field("shared").unwrap();
    assert!(matches!(shared.pointee(), Some(None)));
    // `pointee_make_mut` unshares it by cloning, like `Rc::make_mut`.
    let shared = Mut::new(&mut s).field("shared").unwrap();
    *shared
        .pointee_make_mut()
        .unwrap()
        .field("value")
        .unwrap()
        .downcast::<i32>()
        .unwrap() = 30;
    assert_eq!((s.shared.value, other.value), (30, 21));
    assert!(!Rc::ptr_eq(&s.shared, &other));

    let arc = Arc::new(5u8);
    assert_eq!(Ref::new(&arc).pointee().unwrap().downcast::<u8>(), Some(&5));
    let copy = arc.reflect_clone().unwrap();
    assert!(Arc::ptr_eq(&arc, &copy));
    assert_eq!(*Box::<u8>::reflect_default().unwrap(), 0);
}

#[test]
fn strings_and_lists() {
    let mut cow: Cow<'static, str> = Cow::Borrowed("x");
    assert!(Mut::new(&mut cow).set_str("yz"));
    assert!(matches!(cow, Cow::Owned(ref s) if s == "yz"));

    let mut q = VecDeque::from([1i8]);
    let mut m = Mut::new(&mut q);
    assert!(m.resize(3) && m.insert_default(0) && m.remove(1));
    assert_eq!(q, [0, 0, 0]);

    let mut small: SmallVec<u8, 2> = SmallVec::new();
    let mut m = Mut::new(&mut small);
    assert!(m.resize(4));
    assert!(m.reborrow().index(3).unwrap().set(9u8).is_ok());
    assert_eq!(small.as_slice(), [0, 0, 0, 9]);
    assert_eq!(
        type_info::<SmallVec<u8, 2>>().name.to_string(),
        "marlin_containers::SmallVec<u8, 2>"
    );

    let mut array: ArrayVec<u8, 2> = ArrayVec::new();
    let mut m = Mut::new(&mut array);
    assert!(m.resize(2));
    assert!(!m.resize(3), "an ArrayVec never grows past its capacity");
    assert!(!m.insert_default(0));
    assert_eq!(array.len(), 2);

    let mut inline: InlineString<4> = InlineString::new();
    let mut m = Mut::new(&mut inline);
    assert!(m.set_str("héllo"));
    // Truncated at a char boundary.
    assert_eq!(inline.as_str(), "hél");
    assert_eq!(
        type_info::<InlineString<4>>().name.to_string(),
        "marlin_text::InlineString<4>"
    );
}

#[test]
fn math_types() {
    let mut t = Transform {
        translation: Vec3::new(1.0, 2.0, 3.0),
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };
    assert_eq!(get_path::<f32>(&t, "translation.y"), Ok(&2.0));
    assert_eq!(get_path::<f32>(&t, "rotation.w"), Ok(&1.0));
    set_path(&mut t, "rotation.x", 0.5f32).unwrap();
    assert_eq!(t.rotation.x, 0.5);

    let mut m = Mat4::IDENTITY;
    set_path(&mut m, "w_axis.z", -4.0f32).unwrap();
    assert_eq!(m.w_axis, Vec4::new(0.0, 0.0, -4.0, 1.0));
    let names: Vec<_> = type_info::<Vec4>()
        .fields()
        .iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["x", "y", "z", "w"]);
    assert_eq!(type_info::<Quat>().name.to_string(), "marlin_math::Quat");
    assert_eq!(Quat::reflect_default(), Some(Quat::IDENTITY));
    assert_eq!(Transform::reflect_default(), Some(Transform::IDENTITY));
}

#[test]
fn opaque_types_with_proxies() {
    let name = Name::with_number("enemy", 3);
    let proxy = type_info::<Name>().data::<ReflectProxy>().unwrap();
    assert!((proxy.proxy)().is::<String>());
    let text = proxy.to_proxy(Ref::new(&name)).unwrap();
    assert_eq!(text.as_ref().as_str(), Some("enemy_3"));
    let mut other = Name::default();
    assert!(proxy.from_proxy(Mut::new(&mut other), text.as_ref()));
    assert!(other.eq_exact(name));
    // Wrong types are refused.
    assert!(proxy.to_proxy(Ref::new(&1u8)).is_none());
    assert!(!proxy.from_proxy(Mut::new(&mut other), Ref::new(&1u8)));

    let handle = Handle::<Inner>::from_bits(0x0000_0002_0000_0005);
    let proxy = Ref::new(&handle).data::<ReflectProxy>().unwrap();
    let bits = proxy.to_proxy(Ref::new(&handle)).unwrap();
    assert_eq!(bits.downcast::<u64>().ok(), Some(handle.to_proxy()));
    assert_eq!(
        type_info::<Handle<Inner>>().name.to_string(),
        "marlin_containers::Handle<unit::kinds::Inner>"
    );

    // Registering a type also registers its proxy type.
    let mut registry = Registry::new();
    registry.register::<Name>().unwrap();
    assert!(registry.get_by_name("String").is_some());
}

trait Area {
    fn area(&self) -> f32;
}

impl Area for Vec3 {
    fn area(&self) -> f32 {
        self.x * self.y
    }
}

#[test]
fn run_time_type_data() {
    let mut registry = Registry::new();
    let v = Vec3::new(2.0, 3.0, 0.0);
    let info = type_info::<Vec3>();
    assert!(registry.data::<ReflectTrait<dyn Area>>(info).is_none());
    registry
        .add_data(info, reflect_trait!(Vec3 as dyn Area))
        .unwrap();
    // Added data registers the type.
    assert!(registry.get(info.id).is_some());
    let cast = registry.data::<ReflectTrait<dyn Area>>(info).unwrap();
    assert_eq!(cast.get(Ref::new(&v)).unwrap().area(), 6.0);
    // Adding data of the same type replaces it.
    registry.add_data(info, 5u32).unwrap();
    registry.add_data(info, 6u32).unwrap();
    assert_eq!(registry.data::<u32>(info), Some(&6));

    // The global registry, through `Ref::cast`.
    assert!(Ref::new(&v).cast::<dyn Area>().is_none());
    marlin_reflect::add_data(info, reflect_trait!(Vec3 as dyn Area)).unwrap();
    assert_eq!(Ref::new(&v).cast::<dyn Area>().unwrap().area(), 6.0);
    assert!(marlin_reflect::find_data::<ReflectTrait<dyn Area>>(info).is_some());
}

#[test]
fn id_collision() {
    #[derive(Reflect)]
    #[reflect(name = "kinds::Same")]
    struct A;
    #[derive(Reflect)]
    #[reflect(name = "kinds::Same")]
    struct B;

    let mut registry = Registry::new();
    registry.register::<A>().unwrap();
    let err: IdCollision = registry.register::<B>().unwrap_err();
    assert_eq!(err.id, TypeId::of_name("kinds::Same"));
    assert_eq!(
        (err.existing.as_str(), err.new.as_str()),
        ("kinds::Same", "kinds::Same")
    );
    let message = err.to_string();
    assert!(message.contains("kinds::Same") && message.contains("#[reflect(name"));
    // Nested collisions are found too, and earlier types stay registered.
    #[derive(Reflect)]
    struct Outer {
        a: u8,
        b: B,
    }
    let err = registry.register::<Outer>().unwrap_err();
    assert_eq!(err.id, TypeId::of_name("kinds::Same"));
    assert!(registry.get(type_info::<Outer>().id).is_some());
    // Adding data to a colliding type fails the same way.
    assert!(registry.add_data(type_info::<B>(), 1u8).is_err());
}

#[test]
fn registry_walks_new_kinds() {
    let mut registry = Registry::new();
    registry.register::<Scene>().unwrap();
    for name in [
        "(u8, String)",
        "BTreeMap<String, u32>",
        "BTreeSet<u16>",
        "HashMap<u32, Vec<f32>>",
        "HashSet<String>",
        "Box<unit::kinds::Inner>",
        "Rc<unit::kinds::Inner>",
        "unit::kinds::Inner",
        "VecDeque<i8>",
        "f32",
    ] {
        assert!(registry.get_by_name(name).is_some(), "{name}");
    }
}

#[test]
fn clone_and_default_of_containers() {
    let s = scene();
    let copy = s.reflect_clone().unwrap();
    assert_eq!(copy, s);
    // Cloning an `Rc` shares it.
    assert!(Rc::ptr_eq(&copy.shared, &s.shared));
    let value = Value::clone_of(Ref::new(&s)).unwrap();
    assert_eq!(value.downcast::<Scene>().unwrap(), s);
    let empty = Scene::reflect_default().unwrap();
    assert!(empty.names.is_empty() && empty.lookup.is_empty() && empty.boxed.value == 0);
}
