//! `marlin_front_syntax::parse`: the shared C#-family lexer, parser and validation on any text.
//!
//! Input: a selector byte (low bit: whether the dialect reserves `half`), then the source as
//! UTF-8. The tree must hold exactly the input text, and every error range must lie on character
//! boundaries inside it. The replay test is `fuzz_finds_stay_fixed` in
//! compiler/front/syntax/tests/unit/robustness.rs, which decodes inputs the same way.
#![no_main]

use marlin_front_syntax::{Dialect, parse};

const DIALECTS: [Dialect; 2] = [
    Dialect::SUPERSET,
    Dialect {
        extra_keywords: &[],
        ..Dialect::SUPERSET
    },
];

fn run(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    let parse = parse(text, &DIALECTS[usize::from(selector & 1)]);
    assert_eq!(parse.syntax_node().text(), text);
    for error in parse.errors() {
        let range = error.range.to_range();
        assert!(range.end <= text.len());
        assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
    }
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
