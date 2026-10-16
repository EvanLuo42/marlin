use std::fmt::Write;

use marlin_text::fmt::{SliceWriter, format_to};
use marlin_text::format_to;

#[test]
fn fits() {
    let mut buf = [0u8; 32];
    assert_eq!(
        format_to!(&mut buf, "{} + {} = {}", 2, 2, 4),
        Ok("2 + 2 = 4")
    );
    let mut empty = [0u8; 0];
    assert_eq!(format_to!(&mut empty, ""), Ok(""));
    assert_eq!(format_to(&mut buf, format_args!("{:>5}", "x")), Ok("    x"));
}

#[test]
fn truncates_at_char_boundary() {
    let mut buf = [0u8; 7];
    // "ab" + "名名" = 2 + 6 bytes; only one 名 fits in the remaining 5.
    assert_eq!(format_to!(&mut buf, "ab{}", "名名"), Err("ab名"));
    let mut buf = [0u8; 3];
    assert_eq!(
        format_to!(&mut buf, "{}{}", "abcd", "never written"),
        Err("abc")
    );
}

#[test]
fn writer_stops_after_truncation() {
    let mut buf = [0u8; 4];
    let mut w = SliceWriter::new(&mut buf);
    assert!(w.write_str("ab").is_ok());
    assert!(w.write_str("cde").is_err());
    assert!(w.is_truncated());
    // Nothing more is accepted, even if it would fit in what is left.
    assert!(w.write_str("").is_err());
    assert_eq!(w.as_str(), "abcd");
    assert_eq!(w.len(), 4);
    assert_eq!(w.into_str(), "abcd");
}
