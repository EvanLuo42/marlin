//! Tuples, maps, sets, pointers, engine types, proxies and custom serializers.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use marlin_containers::{Handle, HashMap, HashMapExt, HashSet, HashSetExt, SmallVec};
use marlin_math::{Aabb, Quat, Transform, Vec3};
use marlin_reflect::{Mut, Ref, Reflect, TypeId, TypeInfo, TypeKind, TypeName};
use marlin_serial::{CustomSerial, Error, SerialFns, from_bytes, read_into, to_bytes};
use marlin_text::{InlineString, Name};

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Level {
    spawn: (f32, f32, String),
    scores: BTreeMap<String, u32>,
    lookup: HashMap<u16, Vec<i8>>,
    tags: BTreeSet<u8>,
    seen: HashSet<String>,
    boss: Box<Enemy>,
    shared: Rc<Enemy>,
    atomic: Arc<Vec<u8>>,
    nested: Option<Box<Option<u8>>>,
    transform: Transform,
    bounds: Aabb,
    name: Name,
    label: InlineString<8>,
    target: Handle<Enemy>,
    small: SmallVec<u16, 2>,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Enemy {
    hp: u32,
}

fn level() -> Level {
    let mut lookup = <HashMap<u16, Vec<i8>> as HashMapExt>::new();
    for i in 0..20 {
        lookup.insert(i * 7, vec![i as i8, -1]);
    }
    let mut seen = <HashSet<String> as HashSetExt>::new();
    for s in ["a", "bb", "ccc", "dddd"] {
        seen.insert(s.into());
    }
    Level {
        spawn: (1.5, -2.0, "gate".into()),
        scores: BTreeMap::from([("ana".into(), 3), ("bo".into(), 9)]),
        lookup,
        tags: BTreeSet::from([4, 1, 9]),
        seen,
        boss: Box::new(Enemy { hp: 500 }),
        shared: Rc::new(Enemy { hp: 7 }),
        atomic: Arc::new(vec![1, 2, 3]),
        nested: Some(Box::new(Some(5))),
        transform: Transform {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_xyzw(0.0, 0.6, 0.0, 0.8),
            scale: Vec3::splat(2.0),
        },
        bounds: Aabb::new(Vec3::ZERO, Vec3::ONE),
        name: Name::with_number("Boss", 2),
        label: InlineString::try_from("hi").unwrap(),
        target: Handle::from_bits(0x0003_0000_0011),
        small: SmallVec::from([1u16, 2, 3]),
    }
}

#[test]
fn round_trip_new_kinds() {
    let value = level();
    let bytes = to_bytes(&value).unwrap();
    let back: Level = from_bytes(&bytes).unwrap();
    assert_eq!(back.spawn, value.spawn);
    assert_eq!(back.scores, value.scores);
    assert_eq!(back.lookup, value.lookup);
    assert_eq!(back.tags, value.tags);
    assert_eq!(back.seen, value.seen);
    assert_eq!(back.boss, value.boss);
    assert_eq!(back.shared, value.shared);
    assert_eq!(back.atomic, value.atomic);
    assert_eq!(back.nested, value.nested);
    assert_eq!(back.transform, value.transform);
    assert_eq!(back.bounds, value.bounds);
    assert!(back.name.eq_exact(value.name));
    assert_eq!(back.label, value.label);
    assert_eq!(back.target, value.target);
    assert_eq!(back.small, value.small);
}

#[test]
fn hash_containers_encode_canonically() {
    // The same contents inserted in another order (so likely iterating differently).
    let a = level();
    let mut b = level();
    let mut lookup = <HashMap<u16, Vec<i8>> as HashMapExt>::new();
    for i in (0..20).rev() {
        lookup.insert(i * 7, vec![i as i8, -1]);
    }
    b.lookup = lookup;
    let mut seen = <HashSet<String> as HashSetExt>::new();
    for s in ["dddd", "ccc", "bb", "a"] {
        seen.insert(s.into());
    }
    b.seen = seen;
    assert_eq!(to_bytes(&a).unwrap(), to_bytes(&b).unwrap());
}

/// Version 2 of `Level`'s first fields, with changed container kinds.
#[derive(Reflect, Debug, Default, PartialEq)]
#[reflect(name = "kinds::Converted")]
struct ConvertedA {
    tags: Vec<u8>,
    point: (f32, f32),
    boxed: Box<u32>,
    name: String,
    handle: u64,
}

#[derive(Reflect, Debug, Default, PartialEq)]
#[reflect(name = "kinds::Converted")]
struct ConvertedB {
    tags: BTreeSet<u8>,
    point: Point,
    boxed: u32,
    name: Name,
    handle: Handle<Enemy>,
}

#[derive(Reflect, Debug, Default, PartialEq)]
struct Point(f32, f32);

#[test]
fn conversions_between_kinds() {
    let a = ConvertedA {
        tags: vec![3, 1, 3],
        point: (1.0, 2.0),
        boxed: Box::new(9),
        name: "door_4".into(),
        handle: 0x0001_0000_0002,
    };
    let b: ConvertedB = from_bytes(&to_bytes(&a).unwrap()).unwrap();
    // A list becomes a set, a tuple a tuple struct, a box its pointee; a string reads into a
    // `Name` and a `u64` into a `Handle` through their proxies.
    assert_eq!(b.tags, BTreeSet::from([1, 3]));
    assert_eq!(b.point, Point(1.0, 2.0));
    assert_eq!(b.boxed, 9);
    assert!(b.name.eq_exact(Name::new("door_4")));
    assert_eq!(b.handle.to_bits(), 0x0001_0000_0002);

    let back: ConvertedA = from_bytes(&to_bytes(&b).unwrap()).unwrap();
    assert_eq!(back.tags, [1, 3]);
    assert_eq!(back.point, (1.0, 2.0));
    assert_eq!(*back.boxed, 9);
    assert_eq!(back.name, "door_4");
    assert_eq!(back.handle, 0x0001_0000_0002);
}

#[test]
fn reading_into_a_shared_rc_unshares_it() {
    let mut value = level();
    let other = Rc::clone(&value.shared);
    let mut source = level();
    source.shared = Rc::new(Enemy { hp: 99 });
    read_into(Mut::new(&mut value), &to_bytes(&source).unwrap()).unwrap();
    assert_eq!((value.shared.hp, other.hp), (99, 7));
}

#[test]
fn maps_with_struct_keys_and_corruption() {
    let mut map = BTreeMap::new();
    map.insert((1u8, "a".to_string()), Enemy { hp: 1 });
    map.insert((2u8, "b".to_string()), Enemy { hp: 2 });
    let bytes = to_bytes(&map).unwrap();
    assert_eq!(from_bytes::<BTreeMap<(u8, String), Enemy>>(&bytes), Ok(map));

    let bytes = to_bytes(&level()).unwrap();
    for len in 0..bytes.len() {
        assert!(from_bytes::<Level>(&bytes[..len]).is_err(), "len {len}");
    }
    for i in 0..bytes.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= flip;
            let _ = from_bytes::<Level>(&corrupt);
        }
    }
}

/// An opaque type with a custom serializer declared as static type data.
#[derive(Debug, PartialEq)]
struct Color(u32);

// SAFETY: an opaque description of `Color` itself.
unsafe impl Reflect for Color {
    const TYPE_NAME: TypeName = TypeName::simple("kinds::Color");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&SerialFns::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Color(0))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Color(self.0))
    }
}

impl CustomSerial for Color {
    fn write_bytes(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        // Hex text, to show the bytes are the serializer's own.
        out.extend_from_slice(format!("{:08x}", self.0).as_bytes());
        Ok(())
    }

    fn read_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
        self.0 = u32::from_str_radix(text, 16).map_err(|_| Error::Incompatible)?;
        Ok(())
    }
}

/// An opaque type whose serializer is registered at run time.
#[derive(Debug, PartialEq)]
struct Secret(u8);

// SAFETY: an opaque description of `Secret` itself.
unsafe impl Reflect for Secret {
    const TYPE_NAME: TypeName = TypeName::simple("kinds::Secret");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Opaque);

    fn reflect_default() -> Option<Self> {
        Some(Secret(0))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Secret(self.0))
    }
}

impl CustomSerial for Secret {
    fn write_bytes(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        out.push(self.0 ^ 0x5a);
        Ok(())
    }

    fn read_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let [b] = bytes else {
            return Err(Error::InvalidLength);
        };
        self.0 = b ^ 0x5a;
        Ok(())
    }
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "kinds::Palette")]
struct Palette {
    main: Color,
    all: Vec<Color>,
    keyed: BTreeMap<u8, Color>,
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "kinds::Palette")]
struct PaletteOfInts {
    main: u32,
}

#[test]
fn custom_serializers() {
    let palette = Palette {
        main: Color(0xff00_80ff),
        all: vec![Color(1), Color(2)],
        keyed: BTreeMap::from([(1, Color(0xabc))]),
    };
    let bytes = to_bytes(&palette).unwrap();
    assert!(
        bytes.windows(8).any(|w| w == b"ff0080ff"),
        "custom bytes are embedded as written"
    );
    assert_eq!(from_bytes::<Palette>(&bytes), Ok(palette));
    // Custom data only reads back into the same type: here the field keeps its default.
    assert_eq!(
        from_bytes::<PaletteOfInts>(&bytes),
        Ok(PaletteOfInts { main: 0 })
    );

    // A type without a serializer cannot be written until one is registered.
    let id = TypeId::of_name("kinds::Secret");
    assert_eq!(to_bytes(&Secret(3)), Err(Error::Unsupported(id)));
    marlin_serial::register_custom::<Secret>().unwrap();
    let bytes = to_bytes(&Secret(3)).unwrap();
    assert_eq!(from_bytes::<Secret>(&bytes), Ok(Secret(3)));
    // A bad custom payload is the serializer's error.
    let mut bad = bytes.clone();
    let len_at = bad.len() - 5;
    bad[len_at] = 0;
    bad.pop();
    assert_eq!(from_bytes::<Secret>(&bad), Err(Error::InvalidLength));
    // Writing through `Ref` works the same.
    let mut out = Vec::new();
    marlin_serial::write(Ref::new(&Color(5)), &mut out).unwrap();
    assert_eq!(from_bytes::<Color>(&out), Ok(Color(5)));
}
