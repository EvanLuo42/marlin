//! The StableHash encoding, byte for byte, and `#[derive(StableHash)]`. The encoding is part of
//! cook keys: these expectations must never change.

use std::marker::PhantomData;

use marlin_hash::{
    Crc32, Guid, StableHash, StableHasher, Xxh3, Xxh64, stable_hash_64, stable_hash_128, xxh3_64,
    xxh3_128,
};

/// The encoded bytes of `value`.
fn bytes<T: StableHash + ?Sized>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.stable_hash(&mut out);
    out
}

fn len(n: u64) -> [u8; 8] {
    n.to_le_bytes()
}

#[test]
fn primitives() {
    assert_eq!(bytes(&0x12u8), [0x12]);
    assert_eq!(bytes(&-2i8), [0xfe]);
    assert_eq!(bytes(&0x1234u16), [0x34, 0x12]);
    assert_eq!(bytes(&0x1234_5678u32), [0x78, 0x56, 0x34, 0x12]);
    assert_eq!(bytes(&-1i32), [0xff; 4]);
    assert_eq!(bytes(&1u64), len(1));
    assert_eq!(bytes(&1u128), 1u128.to_le_bytes());
    assert_eq!(bytes(&-1i128), [0xff; 16]);
    // usize / isize are always 8 bytes.
    assert_eq!(bytes(&7usize), len(7));
    assert_eq!(bytes(&-1isize), [0xff; 8]);
    assert_eq!(bytes(&true), [1]);
    assert_eq!(bytes(&false), [0]);
    assert_eq!(bytes(&'é'), 0xe9u32.to_le_bytes());
    assert_eq!(bytes(&1.0f32), 1.0f32.to_bits().to_le_bytes());
    assert_eq!(bytes(&-0.0f64), (1u64 << 63).to_le_bytes());
    assert_ne!(bytes(&0.0f64), bytes(&-0.0f64));
    assert!(bytes(&()).is_empty());
    assert!(bytes(&PhantomData::<String>).is_empty());
}

#[test]
fn strings_and_sequences_are_length_prefixed() {
    let mut expected = len(3).to_vec();
    expected.extend(b"abc");
    assert_eq!(bytes("abc"), expected);
    assert_eq!(bytes(&String::from("abc")), expected);
    assert_eq!(bytes(&b"abc"[..]), expected);

    let mut expected = len(2).to_vec();
    expected.extend([1, 0, 2, 0]);
    assert_eq!(bytes(&[1u16, 2][..]), expected);
    assert_eq!(bytes(&[1u16, 2]), expected, "arrays are prefixed too");
    assert_eq!(bytes(&vec![1u16, 2]), expected);
    assert_eq!(bytes(&Box::new([1u16, 2])), expected);

    // Prefixes keep concatenations apart.
    assert_ne!(bytes(&("ab", "c")), bytes(&("a", "bc")));
    assert_ne!(bytes(&[[1u8], [2]][..]), bytes(&[[1u8, 2]][..]));

    // The slice fast path writes the same bytes as element by element.
    let values: Vec<u64> = (0..100).map(|i| i * 0x0101_0101_0101).collect();
    let mut by_element = len(100).to_vec();
    for v in &values {
        by_element.extend(v.to_le_bytes());
    }
    assert_eq!(bytes(&values), by_element);
    let sizes: Vec<usize> = vec![1, 2];
    let mut expected = len(2).to_vec();
    expected.extend(len(1));
    expected.extend(len(2));
    assert_eq!(bytes(&sizes), expected);
}

#[test]
fn options_results_and_tuples() {
    assert_eq!(bytes(&None::<u8>), len(0));
    let mut some = len(1).to_vec();
    some.push(9);
    assert_eq!(bytes(&Some(9u8)), some);
    assert_eq!(bytes(&Ok::<u8, u8>(9))[..8], len(0));
    assert_eq!(bytes(&Err::<u8, u8>(9))[..8], len(1));
    let mut t = vec![1];
    t.extend([2, 0, 0, 0]);
    t.extend([1]);
    assert_eq!(bytes(&(1u8, 2u32, true)), t);
    assert_eq!(bytes(&&&(1u8, 2u32, true)), t);
    let g = Guid::from_u128(0x0102);
    assert_eq!(bytes(&g), 0x0102u128.to_le_bytes());
}

#[derive(StableHash)]
struct Unit;

#[derive(StableHash)]
struct Tuple(u8, #[stable_hash(skip)] u64, u16);

#[derive(StableHash)]
struct Named<T> {
    id: u32,
    name: String,
    #[stable_hash(skip)]
    cache: Vec<u8>,
    value: T,
}

#[derive(StableHash)]
enum Fieldless {
    A = 3,
    B = -1,
}

#[derive(StableHash)]
#[repr(u8)]
enum WithData<'a> {
    Empty = 0,
    Pair(u8, u8) = 7,
    Named { text: &'a str, flag: bool } = 2,
}

#[derive(StableHash)]
#[repr(u64)]
enum Big {
    Max = u64::MAX,
}

#[derive(StableHash)]
enum Never {}

#[test]
fn derived_structs_hash_fields_in_order() {
    assert!(bytes(&Unit).is_empty());
    let t = Tuple(1, 99, 0x0302);
    assert_eq!(bytes(&t), [1, 2, 3]);
    assert_eq!(t.1, 99);
    let n = Named {
        id: 5,
        name: "hi".into(),
        cache: vec![1, 2, 3],
        value: Some(1u8),
    };
    assert_eq!(n.cache.len(), 3);
    let expected = bytes(&(5u32, "hi", Some(1u8)));
    assert_eq!(bytes(&n), expected);
    let other_cache = Named { cache: vec![], ..n };
    assert_eq!(bytes(&other_cache), expected, "skipped fields do not count");
}

#[test]
fn derived_enums_hash_their_explicit_discriminant() {
    assert_eq!(bytes(&Fieldless::A), len(3));
    assert_eq!(bytes(&Fieldless::B), [0xff; 8]);
    assert_eq!(bytes(&WithData::Empty), len(0));
    let mut pair = len(7).to_vec();
    pair.extend([1, 2]);
    assert_eq!(bytes(&WithData::Pair(1, 2)), pair);
    let mut named = len(2).to_vec();
    named.extend(bytes(&("x", true)));
    assert_eq!(
        bytes(&WithData::Named {
            text: "x",
            flag: true
        }),
        named
    );
    assert_eq!(bytes(&Big::Max), [0xff; 8]);
    let _ = |n: &Never| bytes(n);
}

#[test]
fn sinks_agree_and_values_are_pinned() {
    let value = (
        42u32,
        "Engine/Content/Hero.mesh",
        [1.5f32, -2.0],
        Some(Guid::NIL),
    );
    let encoded = bytes(&value);
    assert_eq!(stable_hash_64(&value), xxh3_64(&encoded));
    assert_eq!(stable_hash_128(&value), xxh3_128(&encoded));
    let mut x64 = Xxh64::new(0);
    StableHash::stable_hash(&value, &mut x64);
    assert_eq!(x64.digest(), marlin_hash::xxh64(&encoded, 0));
    let mut crc = Crc32::new();
    value.stable_hash(&mut crc);
    assert_eq!(crc.finish(), marlin_hash::crc32(&encoded));
    let mut x3 = Xxh3::new();
    let dyn_sink: &mut dyn StableHasher = &mut x3;
    value.stable_hash(dyn_sink);
    assert_eq!(x3.digest(), stable_hash_64(&value));

    assert_eq!(encoded.len(), 4 + 8 + 24 + 8 + 8 + 8 + 16);
    assert_eq!(stable_hash_64(&value), 0x9985_9998_603a_e46b);
    assert_eq!(
        stable_hash_128(&value),
        0x3a03_9043_0354_8cff_d21d_82d8_00f6_31fc
    );
}
