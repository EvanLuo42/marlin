//! Always-on Unicode cases; `unicode_conformance.rs` runs the full UCD test files.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};

use marlin_text::unicode::*;

#[test]
fn simple_case_mapping() {
    assert_eq!(UNICODE_VERSION, (17, 0, 0));
    for (upper, lower) in [
        ('A', 'a'),
        ('É', 'é'),
        ('Ā', 'ā'),
        ('Σ', 'σ'),
        ('Ж', 'ж'),
        ('Ա', 'ա'),
        ('Ა', 'ა'), // Georgian Mtavruli, far from its lowercase
        ('𐐀', '𐐨'), // Deseret, outside the BMP
        ('Ꭰ', 'ꭰ'), // Cherokee lowercases *up* (U+13A0 → U+AB70)
    ] {
        assert_eq!(to_lower(upper), lower, "{upper}");
        assert_eq!(to_upper(lower), upper, "{lower}");
    }
    assert_eq!(to_lower('ς'), 'ς');
    assert_eq!(to_upper('ς'), 'Σ');
    // Simple mappings only: no SpecialCasing.
    assert_eq!(to_upper('ß'), 'ß');
    assert_eq!(to_lower('İ'), 'i');
    assert_eq!(to_lower('1'), '1');
    assert_eq!(to_lower('名'), '名');
    assert_eq!(to_uppercase("straße café"), "STRAßE CAFÉ");
    assert_eq!(to_lowercase("ΟΔΥΣΣΕΥΣ"), "οδυσσευσ");
}

#[test]
fn case_folding() {
    let full = |s: &str| fold(s).collect::<String>();
    let simple = |s: &str| fold_simple(s).collect::<String>();
    assert_eq!(full("Straße"), "strasse");
    assert_eq!(simple("Straße"), "straße");
    assert_eq!(full("ẞ"), "ss");
    assert_eq!(simple("ẞ"), "ß");
    assert_eq!(full("ﬃ"), "ffi");
    assert_eq!(full("ΣΑΣ σας"), "σασ σασ");
    assert_eq!(full("ς"), "σ");
    assert_eq!(full("ǅ"), "ǆ");
    assert_eq!(full("İ"), "i\u{307}");
    assert_eq!(simple_fold('K'), 'k'); // Kelvin sign
    assert_eq!(simple_fold('Ꭰ'), 'Ꭰ'); // Cherokee folds to uppercase
    assert_eq!(simple_fold('ꭰ'), 'Ꭰ');
}

#[test]
fn caseless_comparison() {
    assert!(eq_ignore_case("STRASSE", "straße"));
    assert!(eq_ignore_case("Μάιος", "ΜΆΙΟΣ"));
    assert!(eq_ignore_case("hello", "HeLLo"));
    assert!(!eq_ignore_case("hello", "help"));
    assert!(!eq_ignore_case("e\u{301}", "É"), "not canonical");
    assert!(eq_ignore_case_canonical("e\u{301}", "É"));
    assert!(eq_ignore_case_canonical("ǰ", "J\u{30c}"));
    assert_eq!(cmp_ignore_case("apple", "BANANA"), Ordering::Less);
    assert_eq!(cmp_ignore_case("Ärger", "ärger"), Ordering::Equal);
    let set: HashSet<Caseless> = ["Straße", "STRASSE", "strasse", "Weg"]
        .into_iter()
        .map(Caseless)
        .collect();
    assert_eq!(set.len(), 2);
    let sorted: BTreeSet<Caseless> = ["b", "A", "Ä", "a"].into_iter().map(Caseless).collect();
    assert_eq!(sorted.len(), 3);
    let order: Vec<&str> = sorted.iter().map(|k| k.0).collect();
    assert!(order[0].eq_ignore_ascii_case("a"));
    assert_eq!(order[1..], ["b", "Ä"]);
}

#[test]
fn normalization_forms() {
    let s = |i: Recompositions<_>| i.collect::<String>();
    let d = |i: Decompositions<_>| i.collect::<String>();
    // Canonical ordering of marks: dot below (220) before dot above (230).
    assert_eq!(d(nfd("ṩ")), "s\u{323}\u{307}");
    assert_eq!(s(nfc("s\u{307}\u{323}")), "ṩ");
    assert_eq!(s(nfc("\u{212b}")), "\u{c5}", "singleton Ångström sign");
    assert_eq!(d(nfd("\u{1e0a}\u{323}")), "D\u{323}\u{307}");
    assert_eq!(s(nfc("\u{1e0a}\u{323}")), "\u{1e0c}\u{307}");
    // Hangul, both ways.
    assert_eq!(d(nfd("한")), "\u{1112}\u{1161}\u{11ab}");
    assert_eq!(s(nfc("\u{1112}\u{1161}\u{11ab}")), "한");
    assert_eq!(s(nfc("\u{1100}\u{1161}")), "가");
    // Composition exclusions stay decomposed.
    assert_eq!(s(nfc("\u{0958}")), "\u{915}\u{93c}");
    // Compatibility.
    assert_eq!(normalize("ﬁ", Form::Nfkc), "fi");
    assert_eq!(normalize("①", Form::Nfkd), "1");
    assert_eq!(normalize("ẛ̣", Form::Nfkc), "ṩ");
    assert_eq!(normalize("ẛ̣", Form::Nfc), "ẛ̣");
    assert_eq!(normalize("ｶﾞ", Form::Nfkc), "ガ");
    // A starter blocked by a mark of the same class does not compose.
    assert_eq!(s(nfc("a\u{301}\u{301}")), "á\u{301}");
    // Leading marks, empty input, a long run of marks (sorted with the fallback sort).
    assert_eq!(s(nfc("\u{301}a")), "\u{301}a");
    assert_eq!(s(nfc("")), "");
    let long: String = std::iter::once('a')
        .chain((0..40).flat_map(|_| ['\u{301}', '\u{323}']))
        .collect();
    let normalized = d(nfd(&long));
    assert!(normalized[1..].starts_with(&"\u{323}".repeat(40)));
    assert!(is_nfd(&normalized));
    let mut out = String::new();
    normalize_to("e\u{301}", Form::Nfc, &mut out).unwrap();
    assert_eq!(out, "é");
}

#[test]
fn quick_checks() {
    assert_eq!(quick_check("plain ASCII", Form::Nfc), IsNormalized::Yes);
    assert_eq!(quick_check("é", Form::Nfd), IsNormalized::No);
    assert_eq!(quick_check("e\u{301}", Form::Nfc), IsNormalized::Maybe);
    assert_eq!(
        quick_check("\u{301}\u{323}", Form::Nfd),
        IsNormalized::No,
        "out of order"
    );
    assert_eq!(quick_check("ﬁ", Form::Nfkc), IsNormalized::No);
    assert!(is_nfc("é") && !is_nfc("e\u{301}") && !is_nfc("\u{212b}"));
    assert!(is_nfd("e\u{301}") && !is_nfd("é"));
    assert!(is_nfkc("fi") && !is_nfkc("ﬁ") && is_nfc("ﬁ"));
    assert!(is_nfkd("1") && !is_nfkd("①"));
    assert_eq!(combining_class('\u{301}'), 230);
    assert_eq!(combining_class('\u{323}'), 220);
    assert_eq!(combining_class('a'), 0);
}

#[test]
fn grapheme_clusters() {
    fn g(s: &str) -> Vec<&str> {
        graphemes(s).collect()
    }
    assert_eq!(g(""), Vec::<&str>::new());
    assert_eq!(g("abc"), ["a", "b", "c"]);
    assert_eq!(g("e\u{301}x"), ["e\u{301}", "x"]);
    assert_eq!(g("\r\n\n"), ["\r\n", "\n"]);
    assert_eq!(g("🇫🇷🇩🇪🇮"), ["🇫🇷", "🇩🇪", "🇮"]);
    assert_eq!(g("👨‍👩‍👧x"), ["👨‍👩‍👧", "x"]);
    assert_eq!(g("👍🏽"), ["👍🏽"]);
    assert_eq!(
        g("a\u{200d}👍"),
        ["a\u{200d}", "👍"],
        "GB11 needs a pictograph first"
    );
    assert_eq!(
        g("\u{1100}\u{1161}\u{11a8}가"),
        ["\u{1100}\u{1161}\u{11a8}", "가"]
    );
    assert_eq!(g("नमस्ते"), ["न", "म", "स्", "ते"]);
    // GB9c is not implemented: conjuncts split after the virama.
    assert_eq!(g("क्त"), ["क्", "त"]);
    let indices: Vec<(usize, &str)> = grapheme_indices("ae\u{301}b").collect();
    assert_eq!(indices, [(0, "a"), (1, "e\u{301}"), (4, "b")]);
    let mut it = graphemes("xyz");
    it.next();
    assert_eq!(it.as_str(), "yz");
}

#[test]
fn word_boundaries() {
    fn w(s: &str) -> Vec<&str> {
        word_bounds(s).collect()
    }
    assert_eq!(
        w("The quick (\"brown\") fox can't jump 32.3 feet, right?"),
        [
            "The", " ", "quick", " ", "(", "\"", "brown", "\"", ")", " ", "fox", " ", "can't", " ",
            "jump", " ", "32.3", " ", "feet", ",", " ", "right", "?"
        ]
    );
    assert_eq!(w("e.g. 1,000.5"), ["e.g", ".", " ", "1,000.5"]);
    assert_eq!(w("snake_case x2"), ["snake_case", " ", "x2"]);
    assert_eq!(w("カタカナ漢字"), ["カタカナ", "漢", "字"]);
    assert_eq!(w("a  b"), ["a", "  ", "b"]);
    assert_eq!(w("🇫🇷🇩🇪"), ["🇫🇷", "🇩🇪"]);
    assert_eq!(w("שׁלום\"ם"), ["שׁלום\"ם"]);
    assert_eq!(w("ab\r\ncd"), ["ab", "\r\n", "cd"]);
    let indices: Vec<(usize, &str)> = word_bound_indices("hi there").collect();
    assert_eq!(indices, [(0, "hi"), (2, " "), (3, "there")]);
}
