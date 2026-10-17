use core::error::Error as _;

use marlin_rhi::{Error, ErrorKind};

#[test]
fn display_adds_the_api_code_in_hex() {
    assert_eq!(
        Error::from_kind(ErrorKind::NotFound).to_string(),
        "not found"
    );
    // DXGI_ERROR_DEVICE_REMOVED: negative as an i32, shown as the unsigned HRESULT.
    let removed = Error::new(ErrorKind::DeviceLost, 0x887A_0005_u32 as i32);
    assert_eq!(removed.to_string(), "device lost (0x887a0005)");
    assert_eq!(
        Error::new(ErrorKind::Other, 1).to_string(),
        "other error (0x00000001)"
    );
}

#[test]
fn accessors_and_kind_text() {
    const KIND: &str = ErrorKind::DeviceLost.as_str();
    assert_eq!(KIND, "device lost");
    let e = Error::new(ErrorKind::OutOfMemory, -5);
    assert_eq!(e.kind(), ErrorKind::OutOfMemory);
    assert_eq!(e.raw(), -5);
    assert!(e.source().is_none());
}
