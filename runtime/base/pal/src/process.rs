//! Process control and the process environment: exit, id, command line, executable and working
//! directory, environment variables.
//!
//! Strings come back as UTF-8 written into a caller buffer, since the PAL does not allocate. Each
//! function returns the string's full length in bytes; the buffer holds all of it only when that
//! length is at most `out.len()`, so a caller can size a buffer and retry:
//!
//! ```ignore
//! let mut buf = [0u8; 512];
//! let len = marlin_pal::process::current_exe(&mut buf)?;
//! let exe = core::str::from_utf8(&buf[..len]).unwrap(); // when len <= buf.len()
//! ```
//!
//! Paths are in the OS's own form (`\` separators on Windows, without a `\\?\` prefix).

use crate::backend::imp::Backend;
use crate::contract::Process;
use crate::file::MAX_PATH_LEN;
use crate::{Error, ErrorKind, Result};

/// Longest accepted environment variable name, in UTF-8 bytes.
pub const MAX_ENV_NAME_LEN: usize = 255;

/// Terminates the process abnormally, without running destructors or exit handlers.
#[inline]
pub fn abort() -> ! {
    Backend::abort()
}

/// Terminates the process with `code`, running the C runtime's exit handlers but no Rust
/// destructors (`exit` on POSIX, `ExitProcess` on Windows).
#[inline]
pub fn exit(code: i32) -> ! {
    Backend::exit(code)
}

/// The OS process id.
#[inline]
pub fn id() -> u32 {
    Backend::id()
}

/// Number of command-line arguments, the program name included.
///
/// Read from the OS, not from what a C `main` received: on Windows `GetCommandLineW` split by the
/// MSVC runtime's rules (so arguments are Unicode, unlike the ANSI `argv`); on Linux the `argv`
/// glibc passes to initializers; on macOS `_NSGetArgv`.
#[inline]
pub fn arg_count() -> usize {
    Backend::arg_count()
}

/// Writes command-line argument `index` (0 = the program name) and returns its length, or `None`
/// past the last one. `InvalidData` if the argument is not valid Unicode.
#[inline]
pub fn arg(index: usize, out: &mut [u8]) -> Result<Option<usize>> {
    Backend::arg(index, out)
}

/// The path of the running executable.
#[inline]
pub fn current_exe(out: &mut [u8]) -> Result<usize> {
    Backend::current_exe(out)
}

/// The current working directory.
#[inline]
pub fn current_dir(out: &mut [u8]) -> Result<usize> {
    Backend::current_dir(out)
}

/// Changes the current working directory, which is process-wide. `InvalidInput` for an empty path,
/// one longer than [`MAX_PATH_LEN`] or containing NUL.
pub fn set_current_dir(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_PATH_LEN || path.as_bytes().contains(&0) {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Backend::set_current_dir(path)
}

/// Writes the value of environment variable `name` and returns its length, or `None` if it is
/// not set. `InvalidData` if the value is not valid Unicode; `InvalidInput` for an empty name, one
/// longer than [`MAX_ENV_NAME_LEN`], or one containing NUL or `=`.
///
/// Windows values longer than 8191 UTF-16 units fail with `InvalidInput` (the PAL reads them into
/// a stack buffer).
pub fn env_var(name: &str, out: &mut [u8]) -> Result<Option<usize>> {
    let invalid = name.is_empty()
        || name.len() > MAX_ENV_NAME_LEN
        || name.as_bytes().iter().any(|&b| b == 0 || b == b'=');
    if invalid {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Backend::env_var(name, out)
}
