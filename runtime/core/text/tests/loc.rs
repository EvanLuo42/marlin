//! Locales, plural rules, number and date formatting.

use marlin_text::loc::*;

fn locale(tag: &str) -> Locale {
    Locale::parse(tag).unwrap()
}

#[test]
fn locale_parsing() {
    for (tag, canonical, language, script, region) in [
        ("en", "en", "en", None, None),
        ("pt-BR", "pt-BR", "pt", None, Some("BR")),
        ("zh-Hans-CN", "zh-Hans-CN", "zh", Some("Hans"), Some("CN")),
        ("ZH_hant_tw", "zh-Hant-TW", "zh", Some("Hant"), Some("TW")),
        ("es-419", "es-419", "es", None, Some("419")),
        ("sr-Latn", "sr-Latn", "sr", Some("Latn"), None),
        ("de-DE-u-co-phonebk", "de-DE", "de", None, Some("DE")),
        ("sl-rozaj-biske", "sl", "sl", None, None),
        ("de-CH-1901", "de-CH", "de", None, Some("CH")),
        ("en-US-x-twain", "en-US", "en", None, Some("US")),
        ("yue", "yue", "yue", None, None),
    ] {
        let l = locale(tag);
        assert_eq!(l.to_string(), canonical, "{tag}");
        assert_eq!(
            (l.language(), l.script(), l.region()),
            (language, script, region)
        );
        assert_eq!(locale(canonical), l);
    }
    assert_eq!(Locale::parse(""), Err(LocaleError::Empty));
    assert_eq!(Locale::parse("e"), Err(LocaleError::Language));
    assert_eq!(Locale::parse("english"), Err(LocaleError::Language));
    assert_eq!(Locale::parse("e1"), Err(LocaleError::Language));
    assert_eq!(Locale::parse("en--US"), Err(LocaleError::Subtag(3)));
    assert_eq!(Locale::parse("en-U$"), Err(LocaleError::Subtag(3)));
    assert_eq!(Locale::parse("en-Latn-Latn"), Err(LocaleError::Subtag(8)));
    assert_eq!(Locale::parse("en-US-GB"), Err(LocaleError::Subtag(6)));
    assert_eq!(
        Locale::parse("en-toolongsubtag"),
        Err(LocaleError::Subtag(3))
    );
    assert_eq!("fr-CA".parse::<Locale>(), Ok(locale("fr-CA")));
    assert!(Locale::ROOT.is_root());
    assert_eq!(Locale::default().to_string(), "und");
    assert_eq!(format!("{:?}", locale("pt-BR")), "Locale(\"pt-BR\")");
    assert_eq!(std::mem::size_of::<Locale>(), 10);
}

#[test]
fn fallback_chains() {
    let chain = |tag: &str| -> Vec<String> {
        locale(tag)
            .fallback_chain()
            .map(|l| l.to_string())
            .collect()
    };
    assert_eq!(chain("zh-Hans-CN"), ["zh-Hans-CN", "zh-Hans", "zh"]);
    assert_eq!(chain("pt-BR"), ["pt-BR", "pt"]);
    assert_eq!(chain("en"), ["en"]);
    assert_eq!(chain("sr-Latn-RS"), ["sr-Latn-RS", "sr-Latn", "sr"]);
    // Traditional Chinese never falls back to zh.
    assert_eq!(chain("zh-TW"), ["zh-TW", "zh-Hant"]);
    assert_eq!(chain("zh-HK"), ["zh-HK", "zh-Hant"]);
    assert_eq!(chain("zh-Hant-TW"), ["zh-Hant-TW", "zh-Hant"]);
    assert_eq!(chain("zh-CN"), ["zh-CN", "zh"]);
    assert_eq!(locale("pt-BR").without_region(), locale("pt"));
}

/// CLDR sample lists: `1, 21, 2~4` integers and `0.0~1.5` decimals (step of the last digit).
fn samples(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((a, b)) = item.split_once('~') else {
            out.push(item.to_owned());
            continue;
        };
        let decimals = a.split_once('.').map_or(0, |(_, f)| f.len());
        let scaled = |s: &str| s.replace('.', "").parse::<u64>().unwrap();
        for n in scaled(a)..=scaled(b) {
            let s = n.to_string();
            out.push(if decimals == 0 {
                s
            } else {
                let s = format!("{s:0>width$}", width = decimals + 1);
                let (int, frac) = s.split_at(s.len() - decimals);
                format!("{int}.{frac}")
            });
        }
    }
    out
}

fn check_plurals(tag: &str, expected: &[(PluralCategory, &str)]) {
    let rules = PluralRules::for_locale(&locale(tag));
    let mut seen = Vec::new();
    for &(category, list) in expected {
        for sample in samples(list) {
            let operands = PluralOperands::from_decimal(&sample).unwrap();
            assert_eq!(rules.select(&operands), category, "{tag}: {sample}");
            if !sample.contains('.') {
                let n: i64 = sample.parse().unwrap();
                assert_eq!(rules.select(&PluralOperands::from_int(n)), category);
                assert_eq!(rules.select(&PluralOperands::from_int(-n)), category);
            }
        }
        seen.push(category);
    }
    seen.sort();
    assert_eq!(rules.categories(), seen, "{tag}: categories");
}

use PluralCategory::*;

// The samples are CLDR's own (`@integer` / `@decimal` in plurals.xml).

#[test]
fn plurals_one_if_integer_one() {
    for tag in ["en", "de", "nl", "sv", "fi", "en-GB"] {
        check_plurals(
            tag,
            &[
                (One, "1"),
                (
                    Other,
                    "0, 2~16, 100, 1000, 10000, 100000, 1000000, 0.0~1.5, 10.0, 100.0, 1000.0",
                ),
            ],
        );
    }
}

#[test]
fn plurals_one_if_one() {
    for tag in ["nb", "no", "tr"] {
        check_plurals(
            tag,
            &[
                (One, "1, 1.0, 1.00, 1.000, 1.0000"),
                (
                    Other,
                    "0, 2~16, 100, 1000, 10000, 0.0~0.9, 1.1~1.6, 10.0, 100.0",
                ),
            ],
        );
    }
}

#[test]
fn plurals_danish() {
    check_plurals(
        "da",
        &[
            (One, "1, 0.1~1.6"),
            (
                Other,
                "0, 2~16, 100, 1000, 10000, 0.0, 2.0~3.4, 10.0, 100.0",
            ),
        ],
    );
}

#[test]
fn plurals_romance() {
    check_plurals(
        "es",
        &[
            (One, "1, 1.0, 1.00, 1.000"),
            (Many, "1000000, 2000000, 10000000"),
            (
                Other,
                "0, 2~16, 100, 1000, 10000, 100000, 0.0~0.9, 1.1~1.6, 10.0, 100.0, 1000000.0",
            ),
        ],
    );
    for tag in ["it", "pt-PT", "ca"] {
        check_plurals(
            tag,
            &[
                (One, "1"),
                (Many, "1000000, 3000000"),
                (
                    Other,
                    "0, 2~16, 100, 1000, 10000, 100000, 0.0~1.5, 10.0, 100.0",
                ),
            ],
        );
    }
    for tag in ["fr", "pt", "pt-BR", "fr-CA"] {
        check_plurals(
            tag,
            &[
                (One, "0, 1, 0.0~1.5"),
                (Many, "1000000, 2000000"),
                (
                    Other,
                    "2~17, 100, 1000, 10000, 100000, 2.0~3.5, 10.0, 100.0, 1000000.0",
                ),
            ],
        );
    }
}

#[test]
fn plurals_east_slavic_and_polish() {
    for tag in ["ru", "uk"] {
        check_plurals(
            tag,
            &[
                (One, "1, 21, 31, 41, 51, 61, 71, 81, 101, 1001"),
                (Few, "2~4, 22~24, 32~34, 42~44, 52~54, 62, 102, 1002"),
                (
                    Many,
                    "0, 5~19, 100, 1000, 10000, 100000, 1000000, 111, 112, 114",
                ),
                (Other, "0.0~1.5, 10.0, 100.0, 1000.0, 1.00, 2.10"),
            ],
        );
    }
    check_plurals(
        "pl",
        &[
            (One, "1"),
            (Few, "2~4, 22~24, 32~34, 42~44, 52~54, 62, 102, 1002"),
            (
                Many,
                "0, 5~19, 21, 31, 100, 1000, 10000, 100000, 1000000, 112",
            ),
            (Other, "0.0~1.5, 10.0, 100.0, 1000.0"),
        ],
    );
}

#[test]
fn plurals_czech() {
    check_plurals(
        "cs",
        &[
            (One, "1"),
            (Few, "2~4"),
            (Many, "0.0~1.5, 10.0, 100.0, 1000.0"),
            (Other, "0, 5~19, 100, 1000, 10000, 100000, 1000000, 21"),
        ],
    );
}

#[test]
fn plurals_arabic() {
    check_plurals(
        "ar",
        &[
            (Zero, "0, 0.0, 0.00, 0.000"),
            (One, "1, 1.0, 1.00"),
            (Two, "2, 2.0, 2.00"),
            (Few, "3~10, 103~110, 1003, 3.0, 4.0, 5.0"),
            (Many, "11~26, 111, 1011, 11.0, 12.0, 13.0"),
            (
                Other,
                "100~102, 200~202, 300~302, 400~402, 500~502, 600, 1000, 10000, 0.1~0.9, 1.1~1.7, 10.1, 100.0",
            ),
        ],
    );
}

#[test]
fn plurals_hebrew() {
    check_plurals(
        "he",
        &[
            (One, "1, 0.0~0.9, 0.00~0.05"),
            (Two, "2"),
            (
                Other,
                "0, 3~17, 100, 1000, 10000, 1.0~2.5, 3.0~3.4, 10.0, 100.0",
            ),
        ],
    );
}

#[test]
fn plurals_hindi() {
    check_plurals(
        "hi",
        &[
            (One, "0, 1, 0.0~1.0, 0.00~0.04"),
            (Other, "2~17, 100, 1000, 10000, 1.1~2.6, 10.0, 100.0"),
        ],
    );
}

#[test]
fn plurals_other_only() {
    for tag in ["ja", "ko", "zh", "zh-Hant-TW", "und", "xx"] {
        check_plurals(tag, &[(Other, "0~15, 100, 1000, 10000, 0.0~1.5, 10.0")]);
    }
}

#[test]
fn plural_operands() {
    let o = PluralOperands::from_decimal("-1.50").unwrap();
    assert_eq!((o.i, o.v, o.w, o.f, o.t), (1, 2, 1, 50, 5));
    let o = PluralOperands::from_decimal("120").unwrap();
    assert_eq!((o.i, o.v, o.f), (120, 0, 0));
    for bad in ["", "1.", ".5", "1e3", "1,5", "--1", "1.2.3"] {
        assert_eq!(PluralOperands::from_decimal(bad), None, "{bad:?}");
    }
    assert_eq!(PluralOperands::from_int(-5), PluralOperands::from(5i64));
    assert_eq!(PluralCategory::from_keyword("few"), Some(Few));
    assert_eq!(PluralCategory::from_keyword("several"), None);
    assert_eq!(Many.to_string(), "many");
}

fn int(tag: &str, n: i64) -> String {
    let mut s = String::new();
    NumberFormat::for_locale(&locale(tag))
        .format_int(n, &mut s)
        .unwrap();
    s
}

fn float(tag: &str, x: f64, min: u8, max: u8) -> String {
    let mut s = String::new();
    NumberFormat::for_locale(&locale(tag))
        .format_float(x, min, max, &mut s)
        .unwrap();
    s
}

#[test]
fn number_formatting() {
    const NNBSP: &str = "\u{202f}";
    for (tag, expected) in [
        ("en", "1,234,567.891"),
        ("en-GB", "1,234,567.891"),
        ("de", "1.234.567,891"),
        ("de-AT", "1\u{a0}234\u{a0}567,891"),
        ("de-CH", "1’234’567.891"),
        ("nl", "1.234.567,891"),
        ("sv", "1\u{a0}234\u{a0}567,891"),
        ("da", "1.234.567,891"),
        ("nb", "1\u{a0}234\u{a0}567,891"),
        ("fi", "1\u{a0}234\u{a0}567,891"),
        ("es", "1.234.567,891"),
        ("es-MX", "1,234,567.891"),
        ("it", "1.234.567,891"),
        ("pt", "1.234.567,891"),
        ("pt-PT", "1\u{a0}234\u{a0}567,891"),
        ("fr", &format!("1{NNBSP}234{NNBSP}567,891")),
        ("ru", "1\u{a0}234\u{a0}567,891"),
        ("uk", "1\u{a0}234\u{a0}567,891"),
        ("pl", "1\u{a0}234\u{a0}567,891"),
        ("cs", "1\u{a0}234\u{a0}567,891"),
        ("ar", "١٬٢٣٤٬٥٦٧٫٨٩١"),
        ("he", "1,234,567.891"),
        ("ja", "1,234,567.891"),
        ("ko", "1,234,567.891"),
        ("zh", "1,234,567.891"),
        ("tr", "1.234.567,891"),
        ("hi", "12,34,567.891"),
        ("en-IN", "12,34,567.891"),
        ("xx", "1,234,567.891"),
    ] {
        assert_eq!(float(tag, 1234567.891, 0, 3), expected, "{tag}");
    }
    // Indian grouping: 3 then 2s.
    assert_eq!(int("hi", 1234567890), "1,23,45,67,890");
    assert_eq!(int("hi", 999), "999");
    assert_eq!(int("hi", 1000), "1,000");
    // Minimum grouping digits 2: four-digit numbers stay ungrouped.
    assert_eq!(int("es", 1234), "1234");
    assert_eq!(int("es", 12345), "12.345");
    assert_eq!(int("pl", 1234), "1234");
    assert_eq!(int("pt-PT", 1234), "1234");
    assert_eq!(int("de", 1234), "1.234");
    assert_eq!(int("en", 0), "0");
    assert_eq!(int("en", 999), "999");
    assert_eq!(int("en", i64::MIN), "-9,223,372,036,854,775,808");
    // Minus signs.
    assert_eq!(int("sv", -5), "\u{2212}5");
    assert_eq!(int("he", -5), "\u{200e}-5");
    assert_eq!(int("ar", -5), "\u{61c}-٥");
    // Fractions: rounding, trailing zeros, fixed digits.
    assert_eq!(float("en", 1.23456, 0, 3), "1.235");
    assert_eq!(float("en", 2.0, 0, 3), "2");
    assert_eq!(float("en", 2.0, 2, 2), "2.00");
    assert_eq!(float("en", 0.5, 1, 3), "0.5");
    assert_eq!(float("en", -0.0004, 0, 3), "0");
    assert_eq!(float("en", -1234.5, 0, 0), "-1,234");
    assert_eq!(float("de", -0.25, 0, 1), "-0,2");
    assert_eq!(float("en", f64::NAN, 0, 3), "NaN");
    assert_eq!(float("en", f64::NEG_INFINITY, 0, 3), "-∞");
    assert_eq!(float("en", 1e21, 0, 0), "1,000,000,000,000,000,000,000");
    assert_eq!(float("en", f64::MAX, 20, 20).len(), 309 + 102 + 21);
    assert_eq!(float("en", f64::MAX, 0, 20).len(), 309 + 102);
}

fn date(tag: &str, d: Date, style: DateStyle) -> String {
    let mut s = String::new();
    DateFormat::for_locale(&locale(tag))
        .format_date(d, style, &mut s)
        .unwrap();
    s
}

fn time(tag: &str, t: Time) -> String {
    let mut s = String::new();
    DateFormat::for_locale(&locale(tag))
        .format_time(t, &mut s)
        .unwrap();
    s
}

#[test]
fn date_formatting() {
    let d = Date::new(2024, 1, 5).unwrap();
    let t = Time::new(14, 5, 9).unwrap();
    for (tag, short, long, at) in [
        ("en", "1/5/24", "January 5, 2024", "2:05\u{202f}PM"),
        ("en-GB", "05/01/2024", "5 January 2024", "14:05"),
        ("de", "05.01.24", "5. Januar 2024", "14:05"),
        ("nl", "05-01-2024", "5 januari 2024", "14:05"),
        ("sv", "2024-01-05", "5 januari 2024", "14:05"),
        ("da", "05.01.2024", "5. januar 2024", "14.05"),
        ("nb", "05.01.2024", "5. januar 2024", "14:05"),
        ("fi", "5.1.2024", "5. tammikuuta 2024", "14.05"),
        ("es", "5/1/24", "5 de enero de 2024", "14:05"),
        ("it", "05/01/24", "5 gennaio 2024", "14:05"),
        ("pt", "05/01/2024", "5 de janeiro de 2024", "14:05"),
        ("pt-PT", "05/01/24", "5 de janeiro de 2024", "14:05"),
        ("fr", "05/01/2024", "5 janvier 2024", "14:05"),
        ("ru", "05.01.2024", "5 января 2024 г.", "14:05"),
        ("uk", "05.01.24", "5 січня 2024 р.", "14:05"),
        ("pl", "5.01.2024", "5 stycznia 2024", "14:05"),
        ("cs", "05.01.24", "5. ledna 2024", "14:05"),
        ("ar", "٥\u{200f}/١\u{200f}/٢٠٢٤", "٥ يناير ٢٠٢٤", "٢:٠٥ م"),
        ("he", "5.1.2024", "5 בינואר 2024", "14:05"),
        ("ja", "2024/01/05", "2024年1月5日", "14:05"),
        ("ko", "24. 1. 5.", "2024년 1월 5일", "오후 2:05"),
        ("zh", "2024/1/5", "2024年1月5日", "14:05"),
        ("tr", "5.01.2024", "5 Ocak 2024", "14:05"),
        ("hi", "5/1/24", "5 जनवरी 2024", "2:05 pm"),
        ("xx", "2024-01-05", "2024 M01 5", "14:05"),
    ] {
        assert_eq!(date(tag, d, DateStyle::Short), short, "{tag} short");
        assert_eq!(date(tag, d, DateStyle::Long), long, "{tag} long");
        assert_eq!(time(tag, t), at, "{tag} time");
    }
    assert_eq!(time("en", Time::new(0, 0, 0).unwrap()), "12:00\u{202f}AM");
    assert_eq!(time("en", Time::new(12, 30, 0).unwrap()), "12:30\u{202f}PM");
    let f = DateFormat::for_locale(&locale("en"));
    let mut s = String::new();
    f.format_pattern(
        "yyyy-MM-dd'T'HH:mm:ss '' 'o''clock' Q",
        Some(d),
        Some(t),
        &mut s,
    )
    .unwrap();
    assert_eq!(s, "2024-01-05T14:05:09 ' o'clock Q");
    assert_eq!(Date::new(2024, 2, 29).map(|d| d.day()), Some(29));
    assert_eq!(Date::new(2023, 2, 29), None);
    assert_eq!(Date::new(1900, 2, 29), None);
    assert_eq!(Date::new(2000, 2, 29).map(|d| d.month()), Some(2));
    assert_eq!(Date::new(2024, 13, 1), None);
    assert_eq!(Date::new(2024, 4, 31), None);
    assert_eq!(Time::new(24, 0, 0), None);
}
