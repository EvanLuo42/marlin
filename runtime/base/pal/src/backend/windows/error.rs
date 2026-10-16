use windows_sys::Win32::Foundation::*;
use windows_sys::core::BOOL;

use crate::{Error, ErrorKind, Result};

pub(crate) fn from_win32(code: WIN32_ERROR) -> Error {
    let kind = match code {
        ERROR_NOT_ENOUGH_MEMORY | ERROR_OUTOFMEMORY | ERROR_COMMITMENT_LIMIT => {
            ErrorKind::OutOfMemory
        }
        ERROR_INVALID_PARAMETER | ERROR_INVALID_ADDRESS => ErrorKind::InvalidInput,
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD | ERROR_NOT_ALL_ASSIGNED => {
            ErrorKind::PermissionDenied
        }
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => ErrorKind::NotFound,
        ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS => ErrorKind::AlreadyExists,
        ERROR_TIMEOUT => ErrorKind::TimedOut,
        ERROR_NOT_SUPPORTED | ERROR_CALL_NOT_IMPLEMENTED => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error::new(kind, code as i32)
}

/// The calling thread's last Win32 error.
pub(crate) fn last_error() -> Error {
    // SAFETY: no preconditions.
    from_win32(unsafe { GetLastError() })
}

/// Converts a `BOOL` result, reading the last error on failure.
pub(crate) fn check(ok: BOOL) -> Result<()> {
    if ok != 0 { Ok(()) } else { Err(last_error()) }
}
