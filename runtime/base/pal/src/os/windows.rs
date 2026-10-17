//! Win32 helpers shared with the L1 backends, so every crate maps OS errors the same way.

use crate::{Error, Result};

/// Maps a Win32 error code (`GetLastError`) to an [`Error`].
#[inline]
pub fn from_win32(code: u32) -> Error {
    crate::backend::windows::error::from_win32(code)
}

/// The calling thread's last Win32 error.
#[inline]
pub fn last_error() -> Error {
    crate::backend::windows::error::last_error()
}

/// Converts a Win32 `BOOL` result, reading the last error on failure.
#[inline]
pub fn check(ok: i32) -> Result<()> {
    crate::backend::windows::error::check(ok)
}

/// Milliseconds for a Win32 wait: `INFINITE` for `None`, otherwise rounded up (and capped below
/// `INFINITE`), so a short timeout never turns into a non-blocking poll.
#[inline]
pub fn timeout_ms(timeout: Option<core::time::Duration>) -> u32 {
    const INFINITE: u32 = u32::MAX;
    timeout.map_or(INFINITE, |t| {
        t.as_nanos()
            .div_ceil(1_000_000)
            .min(u128::from(INFINITE - 1)) as u32
    })
}
