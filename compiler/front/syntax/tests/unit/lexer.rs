use marlin_front_syntax::SyntaxKind::{self, *};
use marlin_front_syntax::lex;

/// Non-trivia tokens as (kind, text).
fn tokens(text: &str, extra: &[SyntaxKind]) -> Vec<(SyntaxKind, String)> {
    let (tokens, errors) = lex(text, extra);
    assert!(errors.is_empty(), "{text:?}: {errors:?}");
    let mut offset = 0;
    let mut out = Vec::new();
    for t in tokens {
        let end = offset + t.len.to_usize();
        if !t.kind.is_trivia() {
            out.push((t.kind, text[offset..end].to_string()));
        }
        offset = end;
    }
    assert_eq!(offset, text.len());
    out
}

fn kinds(text: &str) -> Vec<SyntaxKind> {
    tokens(text, &[]).into_iter().map(|(k, _)| k).collect()
}

#[test]
fn keywords_follow_the_dialect() {
    assert_eq!(kinds("half x"), [IDENT, IDENT]);
    assert_eq!(
        tokens("half x", &[HALF_KW])
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<_>>(),
        [HALF_KW, IDENT]
    );
    // Contextual keywords stay identifiers in the lexer; `@` escapes reserved ones.
    assert_eq!(
        kinds("var where class @class"),
        [IDENT, IDENT, CLASS_KW, IDENT]
    );
}

#[test]
fn numbers_lex_by_shape_and_suffix() {
    let cases: &[(&str, SyntaxKind)] = &[
        ("0", INT_NUMBER),
        ("1_000", INT_NUMBER),
        ("0xFF_FFu", INT_NUMBER),
        ("0b1010UL", INT_NUMBER),
        ("3L", INT_NUMBER),
        ("1.5", FLOAT_NUMBER),
        (".5", FLOAT_NUMBER),
        ("1e10", FLOAT_NUMBER),
        ("1.5e-3f", FLOAT_NUMBER),
        ("2h", FLOAT_NUMBER),
        ("2d", FLOAT_NUMBER),
        ("1.5m", FLOAT_NUMBER),
    ];
    for &(text, kind) in cases {
        assert_eq!(kinds(text), [kind], "{text}");
    }
    // A range is not a fraction, and a member access on an integer is not either.
    assert_eq!(kinds("0..n"), [INT_NUMBER, DOT2, IDENT]);
    assert_eq!(kinds("5.Length"), [INT_NUMBER, DOT, IDENT]);
}

#[test]
fn closing_angles_stay_separate() {
    assert_eq!(kinds("A<B<C>>"), [IDENT, LT, IDENT, LT, IDENT, GT, GT]);
    assert_eq!(kinds("a >>= b"), [IDENT, GT, GTEQ, IDENT]);
    assert_eq!(kinds("a <<= b"), [IDENT, SHL_EQ, IDENT]);
}

#[test]
fn strings_and_interpolations_split_into_tokens() {
    assert_eq!(kinds(r#""a\"bA" @"x""y""#), [STRING, STRING]);
    assert_eq!(kinds("'c'"), [CHAR]);
    assert_eq!(
        tokens(r#"$"a {x,4:F2} {{b}} {$"n{y}"}""#, &[]),
        [
            (INTERP_START, "$\"".into()),
            (INTERP_TEXT, "a ".into()),
            (L_CURLY, "{".into()),
            (IDENT, "x".into()),
            (COMMA, ",".into()),
            (INT_NUMBER, "4".into()),
            (COLON, ":".into()),
            (INTERP_FORMAT, "F2".into()),
            (R_CURLY, "}".into()),
            (INTERP_TEXT, " {{b}} ".into()),
            (L_CURLY, "{".into()),
            (INTERP_START, "$\"".into()),
            (INTERP_TEXT, "n".into()),
            (L_CURLY, "{".into()),
            (IDENT, "y".into()),
            (R_CURLY, "}".into()),
            (INTERP_END, "\"".into()),
            (R_CURLY, "}".into()),
            (INTERP_END, "\"".into()),
        ]
    );
    // Braces nested inside a hole do not close it.
    assert_eq!(
        kinds(r#"$"{ F(a[0]) }""#),
        [
            INTERP_START,
            L_CURLY,
            IDENT,
            L_PAREN,
            IDENT,
            L_BRACK,
            INT_NUMBER,
            R_BRACK,
            R_PAREN,
            R_CURLY,
            INTERP_END
        ]
    );
}

#[test]
fn trivia_is_kept_and_bad_input_reports_errors() {
    let (tokens, errors) = lex("/* a */ // b\n# x\n`", &[]);
    let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [
            BLOCK_COMMENT,
            WHITESPACE,
            LINE_COMMENT,
            WHITESPACE,
            ERROR_TOKEN,
            WHITESPACE,
            ERROR_TOKEN
        ]
    );
    assert_eq!(errors.len(), 2);
    assert!(errors[0].message.contains("preprocessor"));
    assert!(errors[1].message.contains("unexpected character"));

    for bad in ["\"abc", "/* abc", "'a", "$\"{x", "1.5q", "0x"] {
        let (tokens, errors) = lex(bad, &[]);
        assert!(!errors.is_empty(), "{bad:?} should report an error");
        let len: usize = tokens.iter().map(|t| t.len.to_usize()).sum();
        assert_eq!(len, bad.len(), "{bad:?}: tokens must cover the input");
    }
}
