//! Robustness: whatever the input, `parse` must not panic, must not blow the stack, and must
//! return a tree whose text is exactly the input. Inputs are seeded random token soups,
//! random characters, mutations of the snapshot files, pathological nesting, and the inputs the
//! `front_syntax_parse` fuzz target found. The binary's `MemCap` bounds memory.

use std::fs;

use marlin_front_syntax::{Dialect, parse};
use marlin_math::Rng;
use marlin_test_support::{data_path, regress, scale, seed};

/// The dialects every input is parsed with: with and without an extra keyword.
const DIALECTS: [Dialect; 2] = [
    Dialect::SUPERSET,
    Dialect {
        extra_keywords: &[],
        ..Dialect::SUPERSET
    },
];

#[rustfmt::skip]
const VOCABULARY: &[&str] = &[
    "struct", "class", "interface", "enum", "namespace", "using", "public", "static", "const",
    "readonly", "in", "out", "ref", "if", "else", "for", "foreach", "while", "do", "switch",
    "case", "default", "break", "continue", "return", "new", "this", "base", "null", "true",
    "var", "where", "type", "extension", "get", "set", "and", "or", "not", "is", "as", "with",
    "operator", "implicit", "half", "float", "int", "float3", "T", "x", "Foo", "_", "{", "}",
    "(", ")", "[", "]", "<", ">", ">>", ">=", "=", "==", "=>", ";", ",", ".", "..", ":", "::",
    "?", "+", "-", "*", "/", "%", "!", "~", "&&", "||", "++", "0", "1.5", "0x1F", "2h", "\"s\"",
    "'c'", "$\"a{", "}b\"", "\"", "@", "#", "//c\n", "/*", "*/", "\n", " ",
];

/// Checks `text`; on failure, shrinks it to a small input that still fails and reports that.
fn check(text: &str, seed: u64) {
    let fails = |t: &str| std::panic::catch_unwind(|| check_once(t)).is_err();
    if !fails(text) {
        return;
    }
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut chars: Vec<char> = text.chars().collect();
    let mut chunk = chars.len().max(1);
    while chunk > 0 {
        let mut i = 0;
        while i < chars.len() {
            let end = (i + chunk).min(chars.len());
            let candidate: String = chars[..i].iter().chain(&chars[end..]).collect();
            if fails(&candidate) {
                chars.drain(i..end);
            } else {
                i += chunk;
            }
        }
        chunk /= 2;
    }
    std::panic::set_hook(previous_hook);
    let small: String = chars.into_iter().collect();
    eprintln!("MARLIN_TEST_SEED={seed}; minimized failing input: {small:?}");
    check_once(&small);
    panic!("seed {seed}: the failure did not reproduce on the minimized input {small:?}");
}

fn check_once(text: &str) {
    for dialect in &DIALECTS {
        let parse = parse(text, dialect);
        assert_eq!(parse.syntax_node().text(), text, "lossy tree for {text:?}");
        for e in parse.errors() {
            let r = e.range.to_range();
            assert!(
                r.end <= text.len()
                    && text.is_char_boundary(r.start)
                    && text.is_char_boundary(r.end),
                "bad error range {r:?} for {text:?}"
            );
        }
    }
}

#[test]
fn token_soups_round_trip() {
    let seed = seed(0x5eed_0001);
    let mut rng = Rng::new(seed);
    for _ in 0..scale(3000) {
        let mut text = String::new();
        for _ in 0..rng.below(200) {
            text.push_str(rng.choose(VOCABULARY).unwrap());
            if rng.chance(0.7) {
                text.push(' ');
            }
        }
        check(&text, seed);
    }
}

#[test]
fn random_characters_round_trip() {
    const CHARS: &[char] = &[
        'a', 'Z', '_', '0', '9', ' ', '\n', '\t', '\r', '{', '}', '(', ')', '[', ']', '<', '>',
        '=', '"', '\'', '$', '@', '#', '\\', '/', '*', '.', ':', ';', ',', 'é', '漢', '\u{feff}',
    ];
    let seed = seed(0x5eed_0002);
    let mut rng = Rng::new(seed);
    for _ in 0..scale(2000) {
        let len = rng.below(120);
        let text: String = (0..len).map(|_| *rng.choose(CHARS).unwrap()).collect();
        check(&text, seed);
    }
}

#[test]
fn mutated_snapshots_round_trip() {
    let mut corpus = Vec::new();
    for sub in ["parser/ok", "parser/err"] {
        let dir = data_path(env!("CARGO_MANIFEST_DIR"), sub);
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "src") {
                corpus.push(fs::read_to_string(path).unwrap());
            }
        }
    }
    let seed = seed(0x5eed_0003);
    let mut rng = Rng::new(seed);
    for source in &corpus {
        let chars: Vec<char> = source.chars().collect();
        for _ in 0..scale(60) {
            let mut mutated = chars.clone();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(mutated.len() + 1);
                match rng.below(4) {
                    0 => {
                        let end = (at + rng.below(20)).min(mutated.len());
                        mutated.drain(at..end);
                    }
                    1 => {
                        let end = (at + rng.below(40)).min(mutated.len());
                        let copy: Vec<char> = mutated[at..end].to_vec();
                        let to = rng.below(mutated.len() + 1);
                        mutated.splice(to..to, copy);
                    }
                    2 => {
                        let token: Vec<char> = rng.choose(VOCABULARY).unwrap().chars().collect();
                        mutated.splice(at..at, token);
                    }
                    _ => mutated.truncate(at),
                }
            }
            check(&mutated.into_iter().collect::<String>(), seed);
        }
    }
}

/// Deep nesting must be reported, not overflow the stack, even on a 1 MiB thread (the main
/// thread of a Windows build script), and must stay fast.
#[test]
fn deep_nesting_is_an_error_not_a_stack_overflow() {
    let cases: Vec<String> = vec![
        format!(
            "void F() {{ x = {}1{}; }}",
            "(".repeat(5000),
            ")".repeat(5000)
        ),
        format!("void F() {{ x = {}1; }}", "-".repeat(5000)),
        format!("void F() {}", "{".repeat(5000)),
        format!("void F() {{ {} }}", "if (a) ".repeat(5000)),
        format!("void F() {{ x = {}1; }}", "a ? b : ".repeat(5000)),
        format!("void F() {{ x = {}1; }}", "y = ".repeat(5000)),
        // Nested type arguments once made the lookahead scan exponential.
        format!("A{} x;", "<A".repeat(5000)),
        format!("void F() {{ x = {}; }}", "[".repeat(5000)),
        format!("void F() {{ x = a{}; }}", ".b".repeat(20000)),
        format!("void F() {{ x = 1{}; }}", " + 1".repeat(20000)),
        format!("void F() {{ x is {}1; }}", "not ".repeat(5000)),
        format!("struct S {{ {} }}", "struct T { ".repeat(5000)),
        format!("var s = {}", "$\"{".repeat(3000)),
    ];
    let worker = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            for text in &cases {
                check_once(text);
            }
        })
        .unwrap();
    marlin_test_support::with_timeout(
        std::time::Duration::from_secs(60),
        "parsing deeply nested input",
        move || worker.join().unwrap(),
    );
}

/// Decodes a `front_syntax_parse` fuzz input: the low bit of the first byte picks the dialect,
/// the rest is the source text.
fn fuzz_input(bytes: &[u8]) -> Option<(&Dialect, &str)> {
    let (&selector, rest) = bytes.split_first()?;
    let text = std::str::from_utf8(rest).ok()?;
    Some((&DIALECTS[usize::from(selector & 1)], text))
}

#[test]
fn fuzz_finds_stay_fixed() {
    regress::for_each(env!("CARGO_MANIFEST_DIR"), "front_syntax_parse", |bytes| {
        if let Some((dialect, text)) = fuzz_input(bytes) {
            let parse = parse(text, dialect);
            assert_eq!(parse.syntax_node().text(), text);
        }
    });
}
