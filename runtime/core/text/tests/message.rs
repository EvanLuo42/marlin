//! Message syntax and formatting.

use marlin_text::loc::MessageErrorKind::*;
use marlin_text::loc::*;

fn locale(tag: &str) -> Locale {
    Locale::parse(tag).unwrap()
}

fn format(tag: &str, message: &str, args: &[Arg]) -> String {
    Message::parse(message)
        .unwrap_or_else(|e| panic!("{message:?}: {e}"))
        .format_to_string(&locale(tag), args)
        .unwrap()
}

fn error(message: &str) -> (usize, MessageErrorKind) {
    let e = Message::parse(message).unwrap_err();
    (e.position, e.kind)
}

#[test]
fn placeholders_and_escapes() {
    let args = [
        Arg::new("name", "Ada"),
        Arg::new("n", 1234567),
        Arg::new("x", 1.23456),
        Arg::new("d", Date::new(2024, 3, 9).unwrap()),
        Arg::new("t", Time::new(9, 30, 0).unwrap()),
    ];
    assert_eq!(format("en", "Hello, {name}!", &args), "Hello, Ada!");
    assert_eq!(format("en", "{ name }", &args), "Ada");
    assert_eq!(format("en", "{n} / {x}", &args), "1,234,567 / 1.235");
    assert_eq!(format("de", "{n} / {x}", &args), "1.234.567 / 1,235");
    assert_eq!(format("en", "{x, number}", &args), "1.235");
    assert_eq!(
        format("en", "{x, number, 1}|{n, number, 2}", &args),
        "1.2|1,234,567.00"
    );
    assert_eq!(
        format("en", "{d}|{d, date}|{d, date, long}", &args),
        "3/9/24|3/9/24|March 9, 2024"
    );
    assert_eq!(
        format("fr", "le {d, date, long} à {t, time}", &args),
        "le 9 mars 2024 à 09:30"
    );
    assert_eq!(format("en", "{t}", &args), "9:30\u{202f}AM");
    assert_eq!(
        format("en", "{{literal}} and }} # {{{name}}}", &args),
        "{literal} and } # {Ada}"
    );
    assert_eq!(format("en", "", &args), "");
    assert_eq!(format("en", "unicode ✓ {name}", &args), "unicode ✓ Ada");
    assert_eq!(format("en", "{a.b-c_1}", &[Arg::new("a.b-c_1", 7u8)]), "7");
}

#[test]
fn plural_selection() {
    let files = "{n, plural, =0 {no files} one {# file} other {# files}}";
    let en = |n: i64| format("en", files, &[Arg::new("n", n)]);
    assert_eq!(en(0), "no files");
    assert_eq!(en(1), "1 file");
    assert_eq!(en(2), "2 files");
    assert_eq!(en(1000), "1,000 files");
    assert_eq!(
        en(-1),
        "-1 file",
        "categories use the absolute value; =N needs N >= 0"
    );
    let x = |x: f64| format("en", files, &[Arg::new("n", x)]);
    assert_eq!(x(1.0), "1 file", "as written: 1.0 is shown as 1");
    assert_eq!(x(1.5), "1.5 files");
    assert_eq!(x(0.0), "no files");
    let ru = "{n, plural, one {# день} few {# дня} many {# дней} other {# дня}}";
    for (n, expected) in [
        (1, "1 день"),
        (3, "3 дня"),
        (5, "5 дней"),
        (11, "11 дней"),
        (21, "21 день"),
    ] {
        assert_eq!(format("ru", ru, &[Arg::new("n", n)]), expected);
    }
    assert_eq!(format("ru", ru, &[Arg::new("n", 1.5)]), "1,5 дня");
    let ar =
        "{n, plural, zero {لا شيء} one {واحد} two {اثنان} few {# قليل} many {# كثير} other {# آخر}}";
    assert_eq!(format("ar", ar, &[Arg::new("n", 0)]), "لا شيء");
    assert_eq!(format("ar", ar, &[Arg::new("n", 3)]), "٣ قليل");
    assert_eq!(format("ar", ar, &[Arg::new("n", 11)]), "١١ كثير");
    assert_eq!(format("ar", ar, &[Arg::new("n", 100)]), "١٠٠ آخر");
    // A category the message lacks falls to `other`; an exact arm beats the category.
    assert_eq!(
        format(
            "ja",
            "{n, plural, one {one} other {# 個}}",
            &[Arg::new("n", 1)]
        ),
        "1 個"
    );
    assert_eq!(
        format(
            "en",
            "{n, plural, one {one} =1 {exactly one} other {many}}",
            &[Arg::new("n", 1)]
        ),
        "exactly one"
    );
    // `##` is a literal `#` in an arm; outside arms `#` is plain text.
    assert_eq!(
        format("en", "#{n, plural, other {## #}}", &[Arg::new("n", 4)]),
        "## 4"
    );
}

#[test]
fn nested_plurals() {
    let m = "{a, plural, one {{b, plural, one {# of #} other {# of {a}}}} other {# outer, {b, plural, other {#}} inner}}";
    let f = |a: i64, b: i64| format("en", m, &[Arg::new("a", a), Arg::new("b", b)]);
    assert_eq!(f(1, 1), "1 of 1");
    assert_eq!(f(1, 7), "7 of 1");
    assert_eq!(f(3, 9), "3 outer, 9 inner");
    let mut walked = Vec::new();
    Message::parse(m).unwrap().walk(|p| walked.push(p)).unwrap();
    use Part::{Argument, Plural};
    use marlin_text::loc::Selector::Category;
    assert_eq!(
        walked,
        [
            Plural("a"),
            Part::Selector(Category(PluralCategory::One)),
            Plural("b"),
            Part::Selector(Category(PluralCategory::One)),
            Part::Selector(Category(PluralCategory::Other)),
            Argument("a"),
            Part::Selector(Category(PluralCategory::Other)),
            Plural("b"),
            Part::Selector(Category(PluralCategory::Other)),
        ]
    );
    let deep = (0..8).fold("x".to_owned(), |inner, _| {
        format!("{{n, plural, other {{{inner}}}}}")
    });
    assert_eq!(format("en", &deep, &[Arg::new("n", 1)]), "x");
    let deeper = format!("{{n, plural, other {{{deep}}}}}");
    assert_eq!(error(&deeper).1, TooDeep);
}

#[test]
fn syntax_errors_have_positions() {
    for (message, position, kind) in [
        ("oops }", 5, UnmatchedClose),
        ("{name", 0, Unclosed),
        ("a {name, number", 2, Unclosed),
        ("{}", 1, ExpectedName),
        ("{ !x}", 2, ExpectedName),
        ("{x y}", 3, ExpectedCommaOrClose),
        ("{x, money}", 4, UnknownFormat),
        ("{x, number, 2 3}", 14, ExpectedClose),
        ("{x, number, 21}", 12, BadStyle),
        ("{x, number, abc}", 12, BadStyle),
        ("{x, date, medium}", 10, BadStyle),
        ("{x, time, short}", 8, ExpectedClose),
        ("{n, plural one {a} other {b}}", 11, ExpectedCommaOrClose),
        ("{n, plural, one {a}}", 19, MissingOther),
        ("{n, plural, other {a} other {b}}", 22, DuplicateSelector),
        (
            "{n, plural, =1 {a} =1 {b} other {c}}",
            19,
            DuplicateSelector,
        ),
        ("{n, plural, several {a} other {b}}", 12, BadSelector),
        ("{n, plural, = {a} other {b}}", 12, BadSelector),
        ("{n, plural, one a other {b}}", 16, ExpectedArm),
        ("{n, plural, other {a", 20, UnclosedArm),
        ("{n, plural, other {a}", 0, Unclosed),
        ("{n, plural, other {{x}", 22, UnclosedArm),
        ("{n, plural, other {a}}}", 22, UnmatchedClose),
    ] {
        assert_eq!(error(message), (position, kind), "{message:?}");
    }
    let e = Message::parse("ab }").unwrap_err();
    assert!(e.to_string().ends_with("at byte 3"), "{e}");
    let many: String = (0..17).map(|i| format!("={i} {{x}} ")).collect();
    assert_eq!(
        error(&format!("{{n, plural, {many}other {{y}}}}")).1,
        TooManyExactSelectors
    );
}

#[test]
fn format_errors() {
    let m = Message::parse("{a} {b, plural, other {#}} {c, date}").unwrap();
    let en = locale("en");
    assert_eq!(
        m.format_to_string(&en, &[]),
        Err(FormatError::MissingArgument("a"))
    );
    assert_eq!(
        m.format_to_string(&en, &[Arg::new("a", 1), Arg::new("b", "x")]),
        Err(FormatError::WrongType("b"))
    );
    assert_eq!(
        m.format_to_string(&en, &[Arg::new("a", 1), Arg::new("b", 2), Arg::new("c", 3)]),
        Err(FormatError::WrongType("c"))
    );
    let bad = Message::new_unchecked("{a");
    assert!(matches!(
        bad.format_to_string(&en, &[Arg::new("a", 1)]),
        Err(FormatError::Message(MessageError { kind: Unclosed, .. }))
    ));
    // A full output buffer is an error, not a panic.
    let mut buf = [0u8; 4];
    let mut w = marlin_text::fmt::SliceWriter::new(&mut buf);
    assert_eq!(
        Message::parse("too long").unwrap().format(&en, &[], &mut w),
        Err(FormatError::Write)
    );
}

#[test]
fn corrupt_messages_never_panic() {
    // xorshift64*, bounded.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let alphabet = [
        '{', '}', '#', ',', ' ', '=', '1', 'n', 'o', 't', 'h', 'e', 'r', 'p', 'l', 'u', 'a', 'é',
    ];
    let words = ["plural", "other", "one", "number", "date", "time", "long"];
    let en = locale("en");
    for _ in 0..20_000 {
        let mut s = String::new();
        for _ in 0..(next() % 24) {
            if next() % 4 == 0 {
                s.push_str(words[(next() % words.len() as u64) as usize]);
            } else {
                s.push(alphabet[(next() % alphabet.len() as u64) as usize]);
            }
        }
        let parsed = Message::parse(&s);
        let result = Message::new_unchecked(&s).format_to_string(&en, &[Arg::new("n", 2)]);
        if parsed.is_ok() {
            assert!(!matches!(result, Err(FormatError::Message(_))), "{s:?}");
        } else {
            assert!(result.is_err(), "{s:?}");
        }
    }
}
