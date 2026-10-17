//! The command line and executable path.

use core::ffi::{CStr, c_char};

use crate::backend::unix::process::copy_utf8;
use crate::{Error, ErrorKind, Result};

/// The process arguments, as dyld recorded them.
pub(crate) fn argv() -> (usize, *const *const c_char) {
    // SAFETY: both return pointers into libSystem's process globals, valid for the process.
    unsafe {
        let argc = *libc::_NSGetArgc();
        let argv = *libc::_NSGetArgv();
        (argc.max(0) as usize, argv.cast_const().cast())
    }
}

pub(crate) fn current_exe(out: &mut [u8]) -> Result<usize> {
    let mut buf = [0u8; crate::file::MAX_PATH_LEN * 4];
    let mut size = buf.len() as u32;
    // SAFETY: `buf` is valid for `size` bytes.
    if unsafe { _NSGetExecutablePath(buf.as_mut_ptr().cast::<c_char>(), &mut size) } != 0 {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    let path = CStr::from_bytes_until_nul(&buf).map_err(|_| Error::from_kind(ErrorKind::Other))?;
    copy_utf8(path.to_bytes(), out)
}

// libc deprecates its binding in favour of the mach2 crate; the function itself is stable dyld API.
unsafe extern "C" {
    fn _NSGetExecutablePath(buf: *mut c_char, bufsize: *mut u32) -> core::ffi::c_int;
}
