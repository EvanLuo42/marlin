//! POSIX helpers shared with the L1 backends, so every crate maps `errno` the same way.

use crate::Error;

/// Maps an `errno` value to an [`Error`].
#[inline]
pub fn from_errno(code: i32) -> Error {
    crate::backend::unix::from_errno(code)
}

/// The calling thread's `errno` as an [`Error`].
#[inline]
pub fn last_error() -> Error {
    crate::backend::unix::last_error()
}

/// Converts the result of a pthread-style function, which returns the error code instead of
/// setting `errno`.
#[inline]
pub fn check_code(code: i32) -> crate::Result<()> {
    crate::backend::unix::check_code(code)
}
