use std::fmt::Write;

use marlin_diag::FixedBuf;

#[test]
fn fits() {
    let mut buf = FixedBuf::<16>::new();
    let (n, s) = (12, "ab");
    write!(buf, "{n}-{s}").unwrap();
    assert_eq!(buf.as_str(), "12-ab");
    assert!(!buf.is_truncated());
    buf.end_line();
    assert_eq!(buf.as_str(), "12-ab\n");
}

#[test]
fn truncates_at_char_boundary() {
    let mut buf = FixedBuf::<5>::new();
    assert!(write!(buf, "aé€").is_err());
    // 'a' (1) + 'é' (2) fit; '€' (3) does not.
    assert_eq!(buf.as_str(), "aé");
    assert!(buf.is_truncated());
    // Further writes are dropped too.
    assert!(buf.write_str("x").is_err());
    assert_eq!(buf.as_str(), "aé");
}

#[test]
fn end_line_marks_truncation() {
    let mut buf = FixedBuf::<8>::new();
    let _ = write!(buf, "0123456789");
    buf.end_line();
    assert_eq!(buf.as_str(), "0123...\n");

    // Exactly full: no room for the newline.
    let mut buf = FixedBuf::<4>::new();
    write!(buf, "abcd").unwrap();
    buf.end_line();
    assert_eq!(buf.as_str(), "...\n");

    // The cut backs off to a char boundary.
    let mut buf = FixedBuf::<7>::new();
    let _ = write!(buf, "ab€€");
    buf.end_line();
    assert_eq!(buf.as_str(), "ab...\n");
}

#[test]
fn clear_resets() {
    let mut buf = FixedBuf::<4>::new();
    let _ = write!(buf, "too long");
    buf.clear();
    assert!(buf.is_empty() && !buf.is_truncated());
    write!(buf, "ok").unwrap();
    assert_eq!(buf.len(), 2);
}
