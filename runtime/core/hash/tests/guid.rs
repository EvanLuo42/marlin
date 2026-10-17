//! Guid: text form, bytes, ordering and name-derived ids.

use marlin_hash::{Guid, ParseGuidError, xxh3_128};

const TEXT: &str = "67e55044-10b1-426f-9247-bb680e5fe0c8";
const VALUE: u128 = 0x67e5_5044_10b1_426f_9247_bb68_0e5f_e0c8;

#[test]
fn parse_and_format_round_trip() {
    let g = Guid::parse(TEXT).unwrap();
    assert_eq!(g.as_u128(), VALUE);
    assert_eq!(g.to_string(), TEXT);
    assert_eq!(format!("{g:?}"), format!("Guid({TEXT})"));
    assert_eq!(format!("{g:>40}"), format!("    {TEXT}"));
    assert_eq!(&g.to_hyphenated(), TEXT.as_bytes());
    assert_eq!(Guid::parse(&TEXT.to_uppercase()), Ok(g));
    assert_eq!(TEXT.parse::<Guid>(), Ok(g));
    assert_eq!(g.version(), 4);

    const PARSED: Guid = match Guid::parse(TEXT) {
        Ok(g) => g,
        Err(_) => panic!(),
    };
    assert_eq!(PARSED, g);

    let nil = Guid::NIL;
    assert!(nil.is_nil());
    assert_eq!(nil, Guid::default());
    assert_eq!(nil.to_string(), "00000000-0000-0000-0000-000000000000");
    assert_eq!(
        Guid::from_u128(u128::MAX).to_string(),
        "ffffffff-ffff-ffff-ffff-ffffffffffff"
    );
}

#[test]
fn rejects_anything_but_the_canonical_form() {
    for bad in [
        "",
        "67e5504410b1426f9247bb680e5fe0c8",
        "{67e55044-10b1-426f-9247-bb680e5fe0c8}",
        "67e55044-10b1-426f-9247-bb680e5fe0c",
        "67e55044-10b1-426f-9247-bb680e5fe0c8a",
        "67e55044_10b1-426f-9247-bb680e5fe0c8",
        "67e5504-410b1-426f-9247-bb680e5fe0c8",
        "67e55044-10b1-426f-9247-bb680e5fe0cg",
        "+7e55044-10b1-426f-9247-bb680e5fe0c8",
        "67e55044-10b1-426f-9247-bb680e5fé0c",
    ] {
        assert_eq!(Guid::parse(bad), Err(ParseGuidError), "{bad:?}");
    }
}

#[test]
fn bytes_are_in_text_order() {
    let g = Guid::from_u128(VALUE);
    let bytes = g.to_bytes();
    assert_eq!(bytes[0], 0x67);
    assert_eq!(bytes[15], 0xc8);
    assert_eq!(Guid::from_bytes(bytes), g);
    // Ord follows the text.
    let mut ids = [
        Guid::parse("ffffffff-0000-0000-0000-000000000000").unwrap(),
        Guid::parse("00000000-0000-0000-0000-000000000001").unwrap(),
        Guid::parse("0000000a-0000-0000-0000-000000000000").unwrap(),
    ];
    ids.sort();
    let mut text: Vec<String> = ids.iter().map(Guid::to_string).collect();
    let sorted = {
        let mut t = text.clone();
        t.sort();
        t
    };
    assert_eq!(text, sorted);
    text.dedup();
    assert_eq!(text.len(), 3);
    assert_eq!(u128::from(g), VALUE);
    assert_eq!(Guid::from(VALUE), g);
}

#[test]
fn from_name_is_deterministic_and_marked_version_8() {
    let ns = Guid::parse(TEXT).unwrap();
    let a = Guid::from_name(ns, b"Engine/Content/Hero.mesh");
    assert_eq!(a, Guid::from_name(ns, b"Engine/Content/Hero.mesh"));
    assert_ne!(a, Guid::from_name(ns, b"Engine/Content/Hero.mesh2"));
    assert_ne!(a, Guid::from_name(Guid::NIL, b"Engine/Content/Hero.mesh"));
    assert_eq!(a.version(), 8);
    assert_eq!(a.to_bytes()[8] >> 6, 0b10, "RFC 9562 variant");

    // The documented construction, and a pinned value: it must never change.
    let mut input = ns.to_bytes().to_vec();
    input.extend_from_slice(b"Engine/Content/Hero.mesh");
    let raw = xxh3_128(&input);
    let mask = (0xf << 76) | (0b11 << 62);
    assert_eq!(a.as_u128(), (raw & !mask) | (0x8 << 76) | (0b10 << 62));
    assert_eq!(a.to_string(), "be278255-ef4b-8ebd-be83-eb35b058c1b8");
}
