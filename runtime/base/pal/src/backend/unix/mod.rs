//! POSIX code shared by the Linux and macOS backends.

mod process;
mod vmem;

use crate::{Error, ErrorKind};

pub(crate) struct Backend;

impl crate::contract::Platform for Backend {}

/// The calling thread's `errno`.
pub(super) fn last_error() -> Error {
    // SAFETY: the errno location is valid for the lifetime of the calling thread.
    #[cfg(target_os = "linux")]
    let code = unsafe { *libc::__errno_location() };
    // SAFETY: as above.
    #[cfg(target_vendor = "apple")]
    let code = unsafe { *libc::__error() };

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
