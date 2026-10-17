use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};

use marlin_text::utf8::*;

#[test]
fn case_insensitive_comparison() {
    assert_eq!(cmp_ignore_ascii_case("abc", "ABD"), Ordering::Less);
    assert_eq!(cmp_ignore_ascii_case("ABC", "abc"), Ordering::Equal);
    assert_eq!(cmp_ignore_ascii_case("ab", "ABC"), Ordering::Less);
    // '_' (0x5f) sorts before 'b' (0x62); a case-sensitive compare would put it after 'B' (0x42).
    assert_eq!(cmp_ignore_ascii_case("_", "B"), Ordering::Less);
    assert!(starts_with_ignore_ascii_case(
        "Textures/Hero.DDS",
        "textures/"
    ));
    assert!(!starts_with_ignore_ascii_case("Tex", "textures/"));
    assert!(ends_with_ignore_ascii_case("Textures/Hero.DDS", ".dds"));
    assert!(!ends_with_ignore_ascii_case("dds", ".dds"));
}

#[test]
fn case_insensitive_search() {
    assert_eq!(find_ignore_ascii_case("Hello World", "WORLD"), Some(6));
    assert_eq!(find_ignore_ascii_case("Hello", ""), Some(0));
    assert_eq!(find_ignore_ascii_case("Hello", "hellos"), None);
    assert_eq!(
        find_ignore_ascii_case("ÀÉ名é", "名É"),
        None,
        "non-ASCII is case-sensitive"
    );
    assert_eq!(find_ignore_ascii_case("ÀÉ名é", "名é"), Some(4));
    assert!(contains_ignore_ascii_case("PlayerController", "controller"));
}

#[test]
fn char_boundaries() {
    let s = "a名b"; // bytes: a | 3 bytes | b
    assert_eq!(floor_char_boundary(s, 0), 0);
    assert_eq!(floor_char_boundary(s, 2), 1);
    assert_eq!(floor_char_boundary(s, 3), 1);
    assert_eq!(floor_char_boundary(s, 4), 4);
    assert_eq!(floor_char_boundary(s, 100), 5);
    assert_eq!(ceil_char_boundary(s, 2), 4);
    assert_eq!(ceil_char_boundary(s, 100), 5);
    assert_eq!(truncate_to(s, 3), "a");
    assert_eq!(truncate_to(s, 4), "a名");
    for (c, w) in [('a', 1), ('é', 2), ('名', 3), ('😀', 4)] {
        let mut buf = [0; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        assert_eq!(char_width(bytes[0]), w);
        if w > 1 {
            assert_eq!(char_width(bytes[1]), 0, "continuation byte");
        }
    }
    assert_eq!(char_width(0xc0), 0, "overlong lead");
    assert_eq!(char_width(0xff), 0);
}

#[test]
fn case_insensitive_keys() {
    let set: HashSet<CaseInsensitive> = ["PNG", "png", "Dds"]
        .into_iter()
        .map(CaseInsensitive)
        .collect();
    assert_eq!(set.len(), 2);
    assert!(set.contains(&CaseInsensitive("dds")));
    let sorted: BTreeSet<CaseInsensitive> =
        ["b", "A", "c"].into_iter().map(CaseInsensitive).collect();
    let order: Vec<&str> = sorted.iter().map(|k| k.0).collect();
    assert_eq!(order, ["A", "b", "c"]);
}
