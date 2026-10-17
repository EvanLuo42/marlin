//! The UCD conformance files, run in full: NormalizationTest.txt, GraphemeBreakTest.txt and
//! WordBreakTest.txt from `$MARLIN_UCD_DIR` (the directory `cargo xtask gen-unicode` reads).
//! Without the variable each test prints why and passes.

use std::path::PathBuf;

use marlin_text::unicode::{
    Form, IsNormalized, UNICODE_VERSION, graphemes, is_normalized, nfc, nfd, nfkc, nfkd, normalize,
    quick_check, word_bounds,
};

fn ucd_file(name: &str) -> Option<String> {
    let Some(dir) = std::env::var_os("MARLIN_UCD_DIR") else {
        println!("MARLIN_UCD_DIR is not set; skipping {name}");
        return None;
    };
    let path = PathBuf::from(dir).join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (major, minor, update) = UNICODE_VERSION;
    let version = format!("{major}.{minor}.{update}");
    assert!(
        text.lines().next().unwrap_or("").contains(&version),
        "{name} is not version {version}"
    );
    Some(text)
}

fn chars(field: &str) -> String {
    field
        .split_whitespace()
        .map(|cp| char::from_u32(u32::from_str_radix(cp, 16).unwrap()).unwrap())
        .collect()
}

#[test]
fn normalization_test() {
    let Some(text) = ucd_file("NormalizationTest.txt") else {
        return;
    };
    let mut cases = 0;
    let mut part1 = std::collections::HashSet::new();
    let mut in_part1 = false;
    for line in text.lines() {
        if let Some(part) = line.strip_prefix('@') {
            in_part1 = part.starts_with("Part1");
            continue;
        }
        let data = line.split('#').next().unwrap();
        if data.trim().is_empty() {
            continue;
        }
        let c: Vec<String> = data.split(';').take(5).map(chars).collect();
        let [c1, c2, c3, c4, c5] = &c[..] else {
            panic!("bad line {line}");
        };
        if in_part1 {
            part1.insert(c1.chars().next().unwrap());
        }
        // c2 == toNFC(c1) == toNFC(c2) == toNFC(c3); c4 == toNFC(c4) == toNFC(c5)
        for (src, nfc_, nfd_, nfkc_, nfkd_) in [
            (c1, c2, c3, c4, c5),
            (c2, c2, c3, c4, c5),
            (c3, c2, c3, c4, c5),
            (c4, c4, c5, c4, c5),
            (c5, c4, c5, c4, c5),
        ] {
            assert_eq!(
                &nfc(src).collect::<String>(),
                nfc_,
                "NFC of {src:?} ({line})"
            );
            assert_eq!(
                &nfd(src).collect::<String>(),
                nfd_,
                "NFD of {src:?} ({line})"
            );
            assert_eq!(
                &nfkc(src).collect::<String>(),
                nfkc_,
                "NFKC of {src:?} ({line})"
            );
            assert_eq!(
                &nfkd(src).collect::<String>(),
                nfkd_,
                "NFKD of {src:?} ({line})"
            );
            for (form, expected) in [
                (Form::Nfc, nfc_),
                (Form::Nfd, nfd_),
                (Form::Nfkc, nfkc_),
                (Form::Nfkd, nfkd_),
            ] {
                assert_eq!(&normalize(src, form), expected, "{form:?} of {src:?}");
                assert_eq!(
                    is_normalized(src, form),
                    src == expected,
                    "is {form:?}: {src:?}"
                );
                let qc = quick_check(src, form);
                assert!(
                    qc == IsNormalized::Maybe || (qc == IsNormalized::Yes) == (src == expected),
                    "quick check {form:?} of {src:?}: {qc:?}"
                );
            }
        }
        cases += 1;
    }
    // Every character not in part 1 is unchanged by every form.
    let mut invariant = 0;
    for c in (0..=0x10ffff).filter_map(char::from_u32) {
        if part1.contains(&c) {
            continue;
        }
        let mut buf = [0; 4];
        let s: &str = c.encode_utf8(&mut buf);
        for form in [Form::Nfc, Form::Nfd, Form::Nfkc, Form::Nfkd] {
            assert!(is_normalized(s, form), "U+{:04X} is not {form:?}", c as u32);
        }
        assert!(nfkd(s).eq([c]), "U+{:04X} decomposes", c as u32);
        invariant += 1;
    }
    println!("NormalizationTest: {cases} lines, {invariant} invariant characters passed");
    assert!(cases > 19_000);
}

/// The cases of a break test file: the string and the expected pieces, and whether the line
/// relies on GB9c (rule 9.3), which is not implemented.
fn break_cases(text: &str) -> Vec<(String, Vec<String>, bool)> {
    let mut cases = Vec::new();
    for line in text.lines() {
        let (data, comment) = line.split_once('#').unwrap_or((line, ""));
        if data.trim().is_empty() {
            continue;
        }
        let mut pieces = Vec::new();
        let mut piece = String::new();
        for token in data.split_whitespace() {
            match token {
                "÷" => {
                    if !piece.is_empty() {
                        pieces.push(std::mem::take(&mut piece));
                    }
                }
                "×" => {}
                cp => piece.push(char::from_u32(u32::from_str_radix(cp, 16).unwrap()).unwrap()),
            }
        }
        assert!(piece.is_empty(), "line does not end with ÷: {line}");
        cases.push((pieces.concat(), pieces, comment.contains("[9.3]")));
    }
    cases
}

#[test]
fn grapheme_break_test() {
    let Some(text) = ucd_file("auxiliary/GraphemeBreakTest.txt") else {
        return;
    };
    let (mut passed, mut skipped) = (0, 0);
    for (s, expected, gb9c) in break_cases(&text) {
        if gb9c {
            skipped += 1;
            continue;
        }
        let got: Vec<&str> = graphemes(&s).collect();
        assert_eq!(got, expected, "{s:?}");
        passed += 1;
    }
    println!("GraphemeBreakTest: {passed} passed, {skipped} skipped (GB9c)");
    assert!(passed > 700);
}

#[test]
fn word_break_test() {
    let Some(text) = ucd_file("auxiliary/WordBreakTest.txt") else {
        return;
    };
    let mut passed = 0;
    for (s, expected, _) in break_cases(&text) {
        let got: Vec<&str> = word_bounds(&s).collect();
        assert_eq!(got, expected, "{s:?}");
        passed += 1;
    }
    println!("WordBreakTest: {passed} passed");
    assert!(passed > 1900);
}
