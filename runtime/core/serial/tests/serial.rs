use marlin_math::Rng;
use marlin_reflect::{Mut, Reflect, Registry, TypeId, TypeInfo, TypeKind, TypeName, name_hash};
use marlin_serial::{
    Error, MAGIC, ReadOptions, VERSION, from_bytes, read_header, read_into, read_into_with,
    read_value, to_bytes,
};

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Everything {
    b: bool,
    c: char,
    u8: u8,
    u16: u16,
    u32: u32,
    u64: u64,
    u128: u128,
    usize: usize,
    i8: i8,
    i16: i16,
    i32: i32,
    i64: i64,
    i128: i128,
    isize: isize,
    f32: f32,
    f64: f64,
    text: String,
    list: Vec<Option<Vec<i16>>>,
    grid: [[u8; 2]; 3],
    maybe: Option<Box3>,
    shapes: Vec<Shape>,
    unit: Unit,
    tuple: Tuple,
    nothing: (),
    #[reflect(transient)]
    scratch: u32,
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
struct Box3 {
    min: [f32; 3],
    max: [f32; 3],
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
struct Unit;

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Tuple(i8, String);

#[derive(Reflect, Debug, Clone, PartialEq)]
enum Shape {
    Empty,
    Circle(f32),
    Rect { w: f32, h: f32 },
    Path(Vec<[f32; 2]>, bool),
}

#[derive(Reflect, Debug, Clone, PartialEq)]
struct Node {
    value: i32,
    children: Vec<Node>,
}

fn everything() -> Everything {
    Everything {
        b: true,
        c: '鱼',
        u8: u8::MAX,
        u16: 0xbeef,
        u32: u32::MAX - 1,
        u64: u64::MAX,
        u128: u128::MAX / 3,
        usize: 123_456,
        i8: i8::MIN,
        i16: -2,
        i32: i32::MIN,
        i64: i64::MIN + 7,
        i128: i128::MIN,
        isize: -99,
        f32: -0.0,
        f64: f64::MAX,
        text: "hello, marlin 🐟".into(),
        list: vec![None, Some(vec![]), Some(vec![1, -1, i16::MAX])],
        grid: [[1, 2], [3, 4], [5, 6]],
        maybe: Some(Box3 {
            min: [-1.0; 3],
            max: [f32::INFINITY, 0.5, 1e-30],
        }),
        shapes: vec![
            Shape::Empty,
            Shape::Circle(2.0),
            Shape::Rect { w: 1.0, h: 2.0 },
            Shape::Path(vec![[0.0, 1.0], [2.0, 3.0]], true),
        ],
        unit: Unit,
        tuple: Tuple(-5, "t".into()),
        nothing: (),
        scratch: 77,
    }
}

#[test]
fn round_trip() {
    let value = everything();
    let bytes = to_bytes(&value).unwrap();
    let back: Everything = from_bytes(&bytes).unwrap();
    // Transient fields are not written and come back as their default.
    assert_eq!(
        back,
        Everything {
            scratch: 0,
            ..value.clone()
        }
    );
    assert!(back.f32.is_sign_negative());

    // Canonical: the same value always encodes to the same bytes.
    assert_eq!(to_bytes(&back).unwrap(), bytes);
    assert_eq!(to_bytes(&value).unwrap(), bytes);
}

#[test]
fn round_trip_nan_and_roots() {
    let bits = from_bytes::<f64>(&to_bytes(&f64::NAN).unwrap()).unwrap();
    assert!(bits.is_nan());
    assert_eq!(
        from_bytes::<String>(&to_bytes(&String::from("x")).unwrap()),
        Ok("x".into())
    );
    assert_eq!(
        from_bytes::<Vec<u8>>(&to_bytes(&vec![1u8, 2]).unwrap()),
        Ok(vec![1, 2])
    );
    assert_eq!(
        from_bytes::<Option<Unit>>(&to_bytes(&Some(Unit)).unwrap()),
        Ok(Some(Unit))
    );
    assert_eq!(
        from_bytes::<[bool; 0]>(&to_bytes(&[true; 0]).unwrap()),
        Ok([])
    );

    let tree = Node {
        value: 1,
        children: vec![
            Node {
                value: 2,
                children: vec![Node {
                    value: 3,
                    children: vec![],
                }],
            },
            Node {
                value: 4,
                children: vec![],
            },
        ],
    };
    assert_eq!(from_bytes::<Node>(&to_bytes(&tree).unwrap()), Ok(tree));
}

#[derive(Reflect, Debug, PartialEq)]
struct Small {
    x: u16,
}

/// Locks down the byte layout, so format changes are deliberate.
#[test]
fn exact_encoding() {
    let bytes = to_bytes(&Small { x: 0x1234 }).unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(b"MRLS");
    expected.extend_from_slice(&[1, 0, 0, 0]);
    expected.extend_from_slice(&TypeId::of_name("serial::Small").0.to_le_bytes());
    expected.push(0x11); // struct
    expected.extend_from_slice(&1u32.to_le_bytes()); // one field
    expected.extend_from_slice(&name_hash("x").to_le_bytes());
    expected.extend_from_slice(&3u32.to_le_bytes()); // signature + value
    expected.push(0x04); // u16
    expected.extend_from_slice(&[0x34, 0x12]);
    assert_eq!(bytes, expected);

    let header = read_header(&bytes).unwrap();
    assert_eq!(header.version, VERSION);
    assert_eq!(header.root, TypeId::of_name("serial::Small"));
}

// Schema evolution: two versions of a type share a stable name.

#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(name = "game::Save")]
struct SaveV1 {
    name: String,
    level: u32,
    gold: i32,
    position: [f32; 2],
    weapon: Weapon1,
    removed: Vec<String>,
    score: u32,
    tags: Vec<String>,
    best: f32,
    friends: Option<u8>,
    retired: Vec<Weapon1>,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(name = "game::Weapon")]
enum Weapon1 {
    Sword { damage: u16 },
    Bow(u8),
    Staff,
}

#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(name = "game::Save", default)]
struct SaveV2 {
    // Reordered.
    position: Vec<f64>,
    // Renamed in Rust; the serialized name stays.
    #[reflect(rename = "name")]
    title: String,
    // Widened.
    gold: i64,
    // Narrowed: 300 no longer fits and falls back to the default.
    level: u8,
    weapon: Weapon2,
    // New field: comes from `Default`.
    speed: f32,
    // Became optional.
    score: Option<u32>,
    // Changed incompatibly: keeps its default.
    tags: u64,
    // Float to integer.
    best: i16,
    // No longer optional.
    friends: u8,
    retired: Vec<Weapon2>,
}

impl Default for SaveV2 {
    fn default() -> Self {
        Self {
            position: vec![],
            title: "nobody".into(),
            gold: 0,
            level: 1,
            weapon: Weapon2::Fists,
            speed: 5.0,
            score: None,
            tags: 99,
            best: 0,
            friends: 3,
            retired: vec![],
        }
    }
}

#[derive(Reflect, Debug, Clone, PartialEq)]
#[reflect(name = "game::Weapon")]
enum Weapon2 {
    #[reflect(default)]
    Fists,
    Sword {
        damage: u32,
        #[reflect(rename = "enchanted")]
        magic: bool,
    },
    Staff,
}

#[test]
fn schema_evolution() {
    let v1 = SaveV1 {
        name: "ana".into(),
        level: 300,
        gold: -20,
        position: [1.5, -2.0],
        weapon: Weapon1::Sword { damage: 40 },
        removed: vec!["gone".into()],
        score: 1000,
        tags: vec!["a".into()],
        best: 12.0,
        friends: Some(7),
        retired: vec![Weapon1::Bow(3), Weapon1::Staff],
    };
    let v2: SaveV2 = from_bytes(&to_bytes(&v1).unwrap()).unwrap();
    assert_eq!(
        v2,
        SaveV2 {
            position: vec![1.5, -2.0],
            title: "ana".into(),
            gold: -20,
            level: 1,
            weapon: Weapon2::Sword {
                damage: 40,
                magic: false
            },
            speed: 5.0,
            score: Some(1000),
            tags: 99,
            best: 12,
            friends: 7,
            // `Bow` was removed: that element becomes the default variant.
            retired: vec![Weapon2::Fists, Weapon2::Staff],
        }
    );

    // And back: fields new in V2 are skipped; V1 needs field-wise defaults.
    let v1_again: SaveV1 = from_bytes(&to_bytes(&v2).unwrap()).unwrap();
    assert_eq!(v1_again.name, "ana");
    assert_eq!(v1_again.gold, -20);
    assert_eq!(v1_again.level, 1);
    assert_eq!(v1_again.position, [1.5, -2.0]);
    assert_eq!(v1_again.weapon, Weapon1::Sword { damage: 40 });
    assert_eq!(v1_again.score, 1000);
    assert_eq!(v1_again.best, 12.0);
    assert_eq!(v1_again.friends, Some(7));
    assert!(v1_again.removed.is_empty());
    assert!(v1_again.tags.is_empty());

    // Out-of-range float to integer and `None` to a non-optional field keep defaults.
    let v1 = SaveV1 {
        best: 0.5,
        friends: None,
        ..v1.clone()
    };
    let v2: SaveV2 = from_bytes(&to_bytes(&v1).unwrap()).unwrap();
    assert_eq!((v2.best, v2.friends), (0, 3));
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Save")]
struct SaveIncompatible(u32);

#[test]
fn root_incompatibility() {
    let bytes = to_bytes(&3u32).unwrap();
    assert_eq!(from_bytes::<String>(&bytes), Err(Error::Incompatible));
    // Numbers convert even at the root.
    assert_eq!(from_bytes::<i64>(&bytes), Ok(3));
    // A struct reads a struct whatever its name: here no field matches.
    let v1 = to_bytes(&Small { x: 1 }).unwrap();
    assert_eq!(from_bytes::<SaveIncompatible>(&v1), Ok(SaveIncompatible(0)));
}

#[test]
fn read_into_overlays() {
    let mut target = Small { x: 9 };
    // An empty struct body leaves fields alone.
    let bytes = to_bytes(&Unit).unwrap();
    read_into(Mut::new(&mut target), &bytes).unwrap();
    assert_eq!(target.x, 9);
    read_into(Mut::new(&mut target), &to_bytes(&Small { x: 4 }).unwrap()).unwrap();
    assert_eq!(target.x, 4);

    // Lists are rebuilt, not merged.
    let mut list = vec![Small { x: 1 }, Small { x: 2 }];
    read_into(Mut::new(&mut list), &to_bytes(&vec![Unit]).unwrap()).unwrap();
    assert_eq!(list, [Small { x: 0 }]);
}

#[test]
fn dynamic_read_through_registry() {
    let mut registry = Registry::new();
    let bytes = to_bytes(&everything()).unwrap();
    assert!(matches!(
        read_value(&bytes, &registry),
        Err(Error::UnknownType(_))
    ));
    registry.register::<Everything>().unwrap();
    let value = read_value(&bytes, &registry).unwrap();
    assert!(value.info().is::<Everything>());
    let text = value
        .as_ref()
        .path("text")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(text, "hello, marlin 🐟");
    assert_eq!(value.downcast::<Everything>().unwrap().u16, 0xbeef);
}

struct Opaque;

// SAFETY: an opaque description of `Opaque` itself.
unsafe impl Reflect for Opaque {
    const TYPE_NAME: TypeName = TypeName::simple("test::Opaque");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Opaque);

    fn reflect_default() -> Option<Self> {
        Some(Opaque)
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Opaque)
    }
}

#[derive(Reflect)]
struct WithOpaque {
    a: u8,
    o: Opaque,
}

#[derive(Reflect)]
struct TransientOpaque {
    a: u8,
    #[reflect(transient)]
    o: Opaque,
}

#[test]
fn unsupported_types() {
    let id = TypeId::of_name("test::Opaque");
    assert_eq!(to_bytes(&Opaque).err(), Some(Error::Unsupported(id)));
    assert_eq!(
        to_bytes(&WithOpaque { a: 1, o: Opaque }).err(),
        Some(Error::Unsupported(id))
    );
    let mut out = vec![1, 2, 3];
    let r = marlin_serial::write(marlin_reflect::Ref::new(&Opaque), &mut out);
    assert!(r.is_err());
    assert_eq!(out, [1, 2, 3]);
    // Transient fields are never touched.
    assert!(to_bytes(&TransientOpaque { a: 1, o: Opaque }).is_ok());
}

// Corrupt input.

/// Every outcome is fine as long as it is not a panic (or worse).
fn try_all<T: Reflect>(bytes: &[u8]) {
    let _ = from_bytes::<T>(bytes);
}

#[test]
fn truncated_input_is_an_error() {
    let bytes = to_bytes(&everything()).unwrap();
    for len in 0..bytes.len() {
        assert!(
            from_bytes::<Everything>(&bytes[..len]).is_err(),
            "len {len}"
        );
    }
}

#[test]
fn corrupted_input_never_panics() {
    let bytes = to_bytes(&everything()).unwrap();
    for i in 0..bytes.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= flip;
            try_all::<Everything>(&corrupt);
        }
    }
    // Pseudo-random multi-byte corruption.
    let mut rng = Rng::new(0x2545_f491_4f6c_dd1d);
    let mut next = || rng.next_u64();
    for _ in 0..20_000 {
        let mut corrupt = bytes.clone();
        for _ in 0..1 + next() % 4 {
            let i = (next() % corrupt.len() as u64) as usize;
            corrupt[i] = next() as u8;
        }
        try_all::<Everything>(&corrupt);
        try_all::<SaveV2>(&corrupt);
    }
}

fn header_for(sig: &[u8]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(sig);
    bytes
}

#[test]
fn specific_corruptions() {
    let good = to_bytes(&Small { x: 1 }).unwrap();

    let mut bad = good.clone();
    bad[0] = b'X';
    assert_eq!(from_bytes::<Small>(&bad), Err(Error::BadMagic));
    assert_eq!(from_bytes::<Small>(b"MR"), Err(Error::BadMagic));

    let mut bad = good.clone();
    bad[4] = 2;
    assert_eq!(from_bytes::<Small>(&bad), Err(Error::UnsupportedVersion(2)));

    let mut bad = good.clone();
    bad.push(0);
    assert_eq!(from_bytes::<Small>(&bad), Err(Error::TrailingBytes));

    // A field whose declared length is longer than its value.
    let mut bad = good.clone();
    let len_at = bad.len() - 7;
    bad[len_at] = 4;
    bad.push(0);
    assert_eq!(from_bytes::<Small>(&bad), Err(Error::InvalidLength));

    let mut bytes = header_for(&[0x01]);
    bytes.push(2);
    assert_eq!(from_bytes::<bool>(&bytes), Err(Error::InvalidBool));

    let mut bytes = header_for(&[0x02]);
    bytes.extend_from_slice(&0xd800u32.to_le_bytes());
    assert_eq!(from_bytes::<char>(&bytes), Err(Error::InvalidChar));

    let mut bytes = header_for(&[0x10]);
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&[0xc3, 0x28]);
    assert_eq!(from_bytes::<String>(&bytes), Err(Error::InvalidUtf8));

    let mut bytes = header_for(&[0x15, 0x03]);
    bytes.push(2);
    assert_eq!(from_bytes::<Option<u8>>(&bytes), Err(Error::InvalidTag));

    assert_eq!(
        from_bytes::<u8>(&header_for(&[0x7f])),
        Err(Error::InvalidSignature)
    );

    // A list claiming four billion elements fails before allocating anything.
    let mut bytes = header_for(&[0x13, 0x0e]);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0; 64]);
    assert_eq!(from_bytes::<Vec<f64>>(&bytes), Err(Error::Truncated));
    let mut bytes = header_for(&[0x14, 0xff, 0xff, 0xff, 0xff, 0x11]);
    bytes.extend_from_slice(&[0; 64]);
    assert_eq!(from_bytes::<Vec<Unit>>(&bytes), Err(Error::Truncated));

    // Same for a struct claiming many fields.
    let mut bytes = header_for(&[0x11]);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(from_bytes::<Small>(&bytes), Err(Error::Truncated));
}

#[test]
fn nesting_is_limited() {
    // Deeply nested data from a small input must not overflow the stack.
    let mut node = Node {
        value: 0,
        children: vec![],
    };
    for i in 1..200 {
        node = Node {
            value: i,
            children: vec![node],
        };
    }
    let bytes = to_bytes(&node).unwrap();
    assert_eq!(from_bytes::<Node>(&bytes), Err(Error::TooDeep));

    let mut target = Node {
        value: 0,
        children: vec![],
    };
    let options = ReadOptions { max_depth: 1000 };
    read_into_with(Mut::new(&mut target), &bytes, &options).unwrap();
    assert_eq!(target, node);

    // Deeply nested signatures too.
    let mut sig = vec![0x15; 10_000];
    sig.push(0x03);
    assert_eq!(from_bytes::<u8>(&header_for(&sig)), Err(Error::TooDeep));
}

struct NoDefault(u8);

// SAFETY: an opaque description of `NoDefault` itself.
unsafe impl Reflect for NoDefault {
    const TYPE_NAME: TypeName = TypeName::simple("test::NoDefault");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Opaque);

    fn reflect_default() -> Option<Self> {
        None
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Self(self.0))
    }
}

/// Has no default because one field has none.
#[derive(Reflect)]
#[reflect(name = "test::Inner")]
struct Inner {
    a: u8,
    nd: NoDefault,
}

#[derive(Reflect)]
#[reflect(name = "test::Inner")]
struct InnerData {
    a: u8,
}

#[derive(Reflect)]
#[reflect(name = "test::Outer")]
struct Outer {
    keep: u8,
    items: Vec<Inner>,
}

#[derive(Reflect)]
#[reflect(name = "test::Outer")]
struct OuterData {
    keep: u8,
    items: Vec<InnerData>,
}

#[test]
fn missing_default() {
    let inner = to_bytes(&InnerData { a: 5 }).unwrap();
    assert_eq!(from_bytes::<Inner>(&inner).err(), Some(Error::NoDefault));
    assert_eq!(
        from_bytes::<NoDefault>(&inner).err(),
        Some(Error::NoDefault)
    );

    // Reading into an existing value needs no default.
    let mut target = Inner {
        a: 0,
        nd: NoDefault(7),
    };
    read_into(Mut::new(&mut target), &inner).unwrap();
    assert_eq!((target.a, target.nd.0), (5, 7));

    // A list of such items cannot grow; the field falls back to its own default (empty), and the
    // rest of the value still loads.
    let outer = to_bytes(&OuterData {
        keep: 9,
        items: vec![InnerData { a: 1 }],
    })
    .unwrap();
    let back: Outer = from_bytes(&outer).unwrap();
    assert_eq!(back.keep, 9);
    assert!(back.items.is_empty());
}
