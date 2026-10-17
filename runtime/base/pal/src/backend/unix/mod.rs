//! POSIX code shared by the Linux and macOS backends. OS-specific pieces live in `../linux` and
//! `../macos` and are reached through `crate::backend::imp`.

mod debug;
#[cfg(marlin_ext = "dylib")]
mod dylib;
mod fiber;
pub(super) mod file;
mod fs;
pub(super) mod process;
mod thread;
mod time;
mod tls;
mod vmem;
#[cfg(marlin_ext = "vmem_alias")]
mod vmem_alias;

use core::ffi::{c_char, c_int};

use crate::file::MAX_PATH_LEN;
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
        libc::ECANCELED => ErrorKind::Cancelled,
        libc::ENOTEMPTY => ErrorKind::DirectoryNotEmpty,
        libc::ENOTDIR => ErrorKind::NotADirectory,
        libc::EISDIR => ErrorKind::IsADirectory,
        libc::EILSEQ => ErrorKind::InvalidData,
        _ => ErrorKind::Other,
    };
    Error::new(kind, code)
}

/// The calling thread's `errno`.
pub(super) fn last_error() -> Error {
    from_errno(errno())
}

/// Sets `errno`, for calls that report errors only through it.
pub(super) fn set_errno(code: c_int) {
    // SAFETY: the errno location is valid for the lifetime of the calling thread.
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = code
    };
    // SAFETY: as above.
    #[cfg(target_vendor = "apple")]
    unsafe {
        *libc::__error() = code
    };
}

/// Copies `path` to a NUL-terminated buffer on the stack for `f`. The caller has checked it (at
/// most `MAX_PATH_LEN` bytes, no NUL).
pub(super) fn with_c_path<R>(path: &str, f: impl FnOnce(*const c_char) -> Result<R>) -> Result<R> {
    let mut c_path = [0u8; MAX_PATH_LEN + 1];
    c_path[..path.len()].copy_from_slice(path.as_bytes());
    f(c_path.as_ptr().cast())
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
