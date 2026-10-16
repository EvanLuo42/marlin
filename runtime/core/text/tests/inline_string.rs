use std::collections::HashSet;
use std::fmt::Write;
use std::mem::size_of;

use marlin_text::{CapacityError, InlineString, format_inline};

#[test]
fn push_and_capacity() {
    let mut s: InlineString<8> = InlineString::new();
    assert_eq!(s.capacity(), 8);
    s.push_str("abc");
    s.push('d');
    assert_eq!(s, "abcd");
    assert_eq!(s.try_push_str("efghi"), Err(CapacityError(())));
    assert_eq!(s, "abcd", "a failed push leaves the string unchanged");
    assert!(s.try_push_str("efgh").is_ok());
    assert_eq!(s.remaining_capacity(), 0);
    assert_eq!(s.try_push('!'), Err(CapacityError('!')));
    assert_eq!(s.pop(), Some('h'));
    s.truncate(2);
    assert_eq!(s.as_str(), "ab");
    s.clear();
    assert!(s.is_empty());
    assert_eq!(size_of::<InlineString<16>>(), 18);
}

#[test]
fn utf8_boundaries() {
    let mut s: InlineString<5> = InlineString::new();
    // "é" is 2 bytes, "名" 3.
    assert!(!s.push_str_truncating("éé名"), "7 bytes do not fit in 5");
    assert_eq!(s, "éé");
    assert!(!s.push_str_truncating("名"));
    assert_eq!(s.len(), 4);
    assert!(s.push_str_truncating("x"));
    assert_eq!(s.pop(), Some('x'));
    assert_eq!(s.pop(), Some('é'));
    assert_eq!(s, "é");
    let mut t: InlineString<6> = "a名b".parse().unwrap();
    t.as_mut_str().make_ascii_uppercase();
    assert_eq!(t, "A名B");
}

#[test]
#[should_panic(expected = "char boundary")]
fn truncate_inside_a_char_panics() {
    let mut s: InlineString<8> = InlineString::try_from("名").unwrap();
    s.truncate(1);
}

#[test]
#[should_panic(expected = "capacity 2 exceeded")]
fn push_past_capacity_panics() {
    let mut s: InlineString<2> = InlineString::new();
    s.push_str("abc");
}

#[test]
fn formatting() {
    let s: InlineString<32> = format_inline!("{}-{:04}", "id", 42).unwrap();
    assert_eq!(s, "id-0042");
    let r: Result<InlineString<4>, _> = format_inline!("{}", 123456);
    assert!(r.is_err());
    let mut w: InlineString<16> = InlineString::new();
    write!(w, "{:?}", [1, 2]).unwrap();
    assert_eq!(w, "[1, 2]");
    assert_eq!(format!("{w}|{w:?}"), "[1, 2]|\"[1, 2]\"");
}

#[test]
fn const_and_traits() {
    const GREETING: InlineString<8> = match InlineString::try_from_str("hello") {
        Ok(s) => s,
        Err(_) => panic!(),
    };
    assert_eq!(GREETING, "hello");
    assert!(InlineString::<4>::try_from_str("hello").is_err());
    let copy = GREETING;
    assert_eq!(copy, GREETING);
    let other: InlineString<64> = "hello".parse().unwrap();
    assert_eq!(GREETING, other);
    let a: InlineString<8> = "a".parse().unwrap();
    assert!(a < GREETING);
    // Borrow<str> lets maps be queried with &str.
    let set: HashSet<InlineString<8>> = [GREETING, a].into_iter().collect();
    assert!(set.contains("hello"));
    assert!(GREETING.starts_with("he"));
}
