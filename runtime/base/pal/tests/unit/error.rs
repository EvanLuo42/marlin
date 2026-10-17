use core::error::Error as _;

use marlin_pal::{Error, ErrorKind};

#[test]
fn display_adds_the_os_code_when_there_is_one() {
    assert_eq!(
        Error::from_kind(ErrorKind::NotFound).to_string(),
        "not found"
    );
    assert_eq!(
        Error::new(ErrorKind::PermissionDenied, 5).to_string(),
        "permission denied (os error 5)"
    );
    assert_eq!(
        Error::new(ErrorKind::Other, -2).to_string(),
        "other error (os error -2)"
    );
}

#[test]
fn accessors_and_kind_text() {
    const KIND: &str = ErrorKind::UnexpectedEof.as_str();
    assert_eq!(KIND, "unexpected end of file");
    assert_eq!(ErrorKind::WouldBlock.to_string(), "operation would block");

    let e = Error::new(ErrorKind::TimedOut, 1460);
    assert_eq!(e.kind(), ErrorKind::TimedOut);
    assert_eq!(e.raw_os_error(), 1460);
    assert!(e.source().is_none());
}
