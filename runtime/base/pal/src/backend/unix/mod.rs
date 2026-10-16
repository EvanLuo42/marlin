//! POSIX code shared by the Linux and macOS backends. OS-specific pieces live in `../linux` and
//! `../macos` and are reached through `crate::backend::imp`.

mod debug;
mod process;
mod thread;
mod time;
mod tls;
mod vmem;

use core::ffi::c_int;

use crate::{Error, ErrorKind, Result};

pub(crate) struct Backend;

impl crate::contract::Platform for Backend {}

pub(super) fn errno() -> c_int {
    // SAFETY: the errno location is valid for the lifetime of the calling thread.
    #[cfg(target_os = "linux")]
    let code = unsafe { *libc::__errno_location() };
    // SAFETY: as above.
    #[cfg(target_vendor = "apple")]
    let code = unsafe { *libc::__error() };
    code
}

pub(super) fn from_errno(code: c_int) -> Error {
    let kind = match code {
        libc::ENOMEM => ErrorKind::OutOfMemory,
        libc::EINVAL => ErrorKind::InvalidInput,
        libc::EACCES | libc::EPERM => ErrorKind::PermissionDenied,
        libc::ENOENT => ErrorKind::NotFound,
        libc::EEXIST => ErrorKind::AlreadyExists,
        libc::EAGAIN => ErrorKind::WouldBlock,
        libc::ETIMEDOUT => ErrorKind::TimedOut,
        libc::EINTR => ErrorKind::Interrupted,
        libc::ENOSYS | libc::ENOTSUP => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error::new(kind, code)
}

/// The calling thread's `errno`.
pub(super) fn last_error() -> Error {
    from_errno(errno())
}

/// Converts the result of a pthread function, which returns the error code instead of setting
/// errno.
pub(super) fn check_code(code: c_int) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(from_errno(code))
    }
}
