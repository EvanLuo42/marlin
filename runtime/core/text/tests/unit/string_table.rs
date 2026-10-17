//! Cooked string tables: build, open, look up, fall back, reject corruption.

use marlin_text::loc::*;

fn locale(tag: &str) -> Locale {
    Locale::parse(tag).unwrap()
}

fn build(tag: &str, entries: &[(&str, &str)]) -> Vec<u8> {
    let mut b = StringTableBuilder::new(locale(tag));
    for (key, message) in entries {
        b.insert(key, message).unwrap();
    }
    b.build().unwrap()
}

const EN: &[(&str, &str)] = &[
    ("menu.start", "Start"),
    ("menu.quit", "Quit"),
    ("hud.coins", "{n, plural, one {# coin} other {# coins}}"),
    ("hud.greeting", "Welcome back, {player}!"),
    ("empty", ""),
];

#[test]
fn build_and_look_up() {
    let bytes = build("en-US", EN);
    let table = StringTable::new(&bytes).unwrap();
    assert_eq!(table.locale(), locale("en-US"));
    assert_eq!(table.len(), EN.len());
    for (key, message) in EN {
        assert_eq!(table.get(key), Some(*message));
    }
    assert_eq!(table.get("menu.Start"), None, "keys are case-sensitive");
    assert_eq!(table.get(""), None);
    assert_eq!(table.get("missing"), None);
    let mut all: Vec<_> = table.iter().collect();
    all.sort();
    let mut expected = EN.to_vec();
    expected.sort();
    assert_eq!(all, expected);
    let mut s = String::new();
    table
        .format("hud.coins", &[Arg::new("n", 1500)], &mut s)
        .unwrap();
    assert_eq!(s, "1,500 coins");
    assert_eq!(
        table.format("nope", &[], &mut s),
        Err(FormatError::UnknownKey)
    );
    let empty = build("ja", &[]);
    let empty = StringTable::new(&empty).unwrap();
    assert!(empty.is_empty() && empty.get("x").is_none());
}

#[test]
fn output_is_deterministic() {
    let mut reversed = EN.to_vec();
    reversed.reverse();
    assert_eq!(build("en", EN), build("en", &reversed));
    assert_ne!(build("en", EN), build("en-GB", EN));
}

#[test]
fn builder_rejects_bad_input() {
    let mut b = StringTableBuilder::new(locale("en"));
    b.insert("a", "x").unwrap();
    assert_eq!(
        b.insert("a", "y"),
        Err(BuildError::DuplicateKey("a".into()))
    );
    assert_eq!(b.insert("", "y"), Err(BuildError::EmptyKey));
    let err = b.insert("broken", "{n, plural, one {x}}").unwrap_err();
    assert_eq!(
        err,
        BuildError::Message {
            key: "broken".into(),
            error: MessageError {
                position: 19,
                kind: MessageErrorKind::MissingOther
            }
        }
    );
    assert_eq!(
        err.to_string(),
        "message `broken`: the plural has no `other` arm at byte 19"
    );
    assert_eq!(b.len(), 1);
}

#[test]
fn catalog_falls_back_with_the_fallback_locale() {
    let fr = build(
        "fr-CA",
        &[(
            "hud.coins",
            "{n, plural, one {# pièce} many {# de pièces} other {# pièces}}",
        )],
    );
    let en = build("en", EN);
    let tables = [
        StringTable::new(&fr).unwrap(),
        StringTable::new(&en).unwrap(),
    ];
    let catalog = Catalog::new(&tables);
    let coins = |n: i64| {
        catalog
            .format_to_string("hud.coins", &[Arg::new("n", n)])
            .unwrap()
    };
    // French: 0 and 1 are singular, and grouping uses a narrow no-break space.
    assert_eq!(coins(0), "0 pièce");
    assert_eq!(coins(1), "1 pièce");
    assert_eq!(coins(1_000_000), "1\u{202f}000\u{202f}000 de pièces");
    // Missing in French: the English message, with English plural rules and numbers.
    let greeting = catalog
        .format_to_string("hud.greeting", &[Arg::new("player", "Zoé")])
        .unwrap();
    assert_eq!(greeting, "Welcome back, Zoé!");
    let (message, table) = catalog.get("menu.quit").unwrap();
    assert_eq!((message, table.locale()), ("Quit", locale("en")));
    assert_eq!(
        catalog.format_to_string("nowhere", &[]),
        Err(FormatError::UnknownKey)
    );
    assert_eq!(catalog.tables().len(), 2);
}

#[test]
fn corrupt_tables_are_rejected() {
    let good = build("en", EN);
    assert!(StringTable::new(&good).is_ok());
    let mut bad = good.clone();
    bad[0] = b'X';
    assert_eq!(StringTable::new(&bad).unwrap_err(), TableError::Magic);
    let mut bad = good.clone();
    bad[4] = 2;
    assert_eq!(StringTable::new(&bad).unwrap_err(), TableError::Version(2));
    assert_eq!(
        StringTable::new(&good[..good.len() - 1]).unwrap_err(),
        TableError::Size
    );
    assert_eq!(StringTable::new(&good[..10]).unwrap_err(), TableError::Size);
    let mut longer = good.clone();
    longer.push(0);
    assert_eq!(StringTable::new(&longer).unwrap_err(), TableError::Size);
    // A wrong hash.
    let mut bad = good.clone();
    bad[32] ^= 1;
    assert!(matches!(StringTable::new(&bad), Err(TableError::Entry(_))));
    // Invalid UTF-8 in the blob.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] = 0xff;
    assert_eq!(StringTable::new(&bad).unwrap_err(), TableError::Utf8);

    // Every single-byte change and truncation is an error or a table that still works.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..4000 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let r = state.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut bytes = good.clone();
        let at = (r as usize) % bytes.len();
        bytes[at] ^= (r >> 32) as u8 | 1;
        bytes.truncate(bytes.len() - (r >> 40) as usize % 4);
        if let Ok(table) = StringTable::new(&bytes) {
            for (key, _) in EN {
                let _ = table.format(
                    key,
                    &[Arg::new("n", 1), Arg::new("player", "x")],
                    &mut String::new(),
                );
            }
        }
    }
}
