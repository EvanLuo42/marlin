use marlin_text::CapacityError;
use marlin_text::utf16::*;

const SAMPLES: &[&str] = &["", "ascii", "é名", "a😀b", "𐐷\u{10ffff}\u{ffff}\u{e000}"];

#[test]
fn encode_matches_core() {
    for s in SAMPLES {
        let expected: Vec<u16> = s.encode_utf16().collect();
        assert_eq!(len(s), expected.len(), "{s:?}");
        assert!(encode(s).eq(expected.iter().copied()));
        let mut buf = vec![0xffff; expected.len()];
        assert_eq!(encode_to(s, &mut buf).unwrap(), &expected[..]);
        let mut buf = vec![0xffff; expected.len() + 1];
        let z = encode_to_nul_terminated(s, &mut buf).unwrap();
        assert_eq!(z.split_last(), Some((&0, &expected[..])));
        assert_eq!(utf8_len(&expected), Ok(s.len()));
        assert_eq!(decode_to_string(&expected).as_deref(), Ok(*s));
        assert_eq!(decode_lossy(&expected), *s);
        let mut bytes = [0u8; 32];
        assert_eq!(decode_to(&expected, &mut bytes), Ok(*s));
    }
}

#[test]
fn encode_reports_short_buffers() {
    let mut buf = [0u16; 2];
    assert_eq!(encode_to("a😀", &mut buf), Err(CapacityError(())));
    assert_eq!(buf, [0, 0], "nothing written");
    assert_eq!(
        encode_to_nul_terminated("ab", &mut buf),
        Err(CapacityError(()))
    );
    assert_eq!(encode_to_nul_terminated("a", &mut buf).unwrap(), [0x61, 0]);
}

#[test]
fn unpaired_surrogates() {
    // Lone high, lone low, high at the end, reversed pair.
    let cases: [(&[u16], usize, u16, &str); 4] = [
        (&[0x61, 0xd800, 0x62], 1, 0xd800, "a\u{fffd}b"),
        (&[0xdc00, 0x61], 0, 0xdc00, "\u{fffd}a"),
        (&[0x61, 0xdbff], 1, 0xdbff, "a\u{fffd}"),
        (&[0xdc00, 0xd800], 0, 0xdc00, "\u{fffd}\u{fffd}"),
    ];
    for (units, index, unit, lossy) in cases {
        let error = Utf16Error { index, unit };
        assert_eq!(validate(units), Err(error));
        assert_eq!(decode_to_string(units), Err(error));
        assert_eq!(utf8_len(units), Err(error));
        assert_eq!(decode_lossy(units), lossy);
        assert_eq!(utf8_len_lossy(units), lossy.len());
        let mut s = String::new();
        decode_lossy_to(units, &mut s).unwrap();
        assert_eq!(s, lossy);
        assert_eq!(
            decode_to(units, &mut [0; 16]),
            Err(DecodeError::Invalid(error))
        );
        // Same as the standard library.
        assert_eq!(String::from_utf16_lossy(units), lossy);
    }
    let errors: Vec<_> = decode(&[0xd800, 0xd800, 0xde00]).collect();
    assert_eq!(
        errors,
        [
            Err(Utf16Error {
                index: 0,
                unit: 0xd800
            }),
            Ok('\u{10200}')
        ]
    );
    assert_eq!(
        Utf16Error {
            index: 3,
            unit: 0xd800
        }
        .to_string(),
        "unpaired surrogate 0xd800 at UTF-16 index 3"
    );
}

#[test]
fn decode_reports_short_buffers() {
    let units: Vec<u16> = "名前".encode_utf16().collect();
    assert_eq!(decode_to(&units, &mut [0; 5]), Err(DecodeError::Capacity));
    assert_eq!(decode_to(&units, &mut [0; 6]), Ok("名前"));
}
