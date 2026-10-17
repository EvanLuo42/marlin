use core::ffi::{CStr, c_char};

use super::{Backend, last_error};
use crate::backend::imp;
use crate::contract::Process;
use crate::file::MAX_PATH_LEN;
use crate::process::MAX_ENV_NAME_LEN;
use crate::{Error, ErrorKind, Result};

impl Process for Backend {
    fn abort() -> ! {
        // SAFETY: no preconditions.
        unsafe { libc::abort() }
    }

    fn exit(code: i32) -> ! {
        // SAFETY: no preconditions.
        unsafe { libc::exit(code) }
    }

    fn id() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getpid() as u32 }
    }

    fn arg_count() -> usize {
        imp::argv().0
    }

    fn arg(index: usize, out: &mut [u8]) -> Result<Option<usize>> {
        let (argc, argv) = imp::argv();
        if index >= argc || argv.is_null() {
            return Ok(None);
        }
        // SAFETY: `argv` holds `argc` NUL-terminated strings that live as long as the process.
        let arg = unsafe { CStr::from_ptr(*argv.add(index)) };
        copy_utf8(arg.to_bytes(), out).map(Some)
    }

    fn current_exe(out: &mut [u8]) -> Result<usize> {
        imp::current_exe(out)
    }

    fn current_dir(out: &mut [u8]) -> Result<usize> {
        let mut buf = [0u8; MAX_PATH_LEN * 4];
        // SAFETY: `buf` is valid for `buf.len()` bytes.
        if unsafe { libc::getcwd(buf.as_mut_ptr().cast(), buf.len()) }.is_null() {
            let error = last_error();
            return Err(if error.raw_os_error() == libc::ERANGE {
                Error::new(ErrorKind::InvalidInput, libc::ERANGE)
            } else {
                error
            });
        }
        let dir =
            CStr::from_bytes_until_nul(&buf).map_err(|_| Error::from_kind(ErrorKind::Other))?;
        copy_utf8(dir.to_bytes(), out)
    }

    fn set_current_dir(path: &str) -> Result<()> {
        let mut c_path = [0u8; MAX_PATH_LEN + 1];
        c_path[..path.len()].copy_from_slice(path.as_bytes());
        // SAFETY: `c_path` is NUL-terminated (the caller rejects interior NULs).
        if unsafe { libc::chdir(c_path.as_ptr().cast::<c_char>()) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    fn env_var(name: &str, out: &mut [u8]) -> Result<Option<usize>> {
        let mut c_name = [0u8; MAX_ENV_NAME_LEN + 1];
        c_name[..name.len()].copy_from_slice(name.as_bytes());
        // SAFETY: `c_name` is NUL-terminated. The PAL never modifies the environment, so the
        // returned string stays valid while we copy it unless the program calls setenv itself.
        let value = unsafe { libc::getenv(c_name.as_ptr().cast::<c_char>()) };
        if value.is_null() {
            return Ok(None);
        }
        // SAFETY: getenv returns a NUL-terminated string.
        let value = unsafe { CStr::from_ptr(value) };
        copy_utf8(value.to_bytes(), out).map(Some)
    }
}

/// Checks that `bytes` is UTF-8 and copies what fits into `out`, returning the full length.
pub(crate) fn copy_utf8(bytes: &[u8], out: &mut [u8]) -> Result<usize> {
    if core::str::from_utf8(bytes).is_err() {
        return Err(Error::from_kind(ErrorKind::InvalidData));
    }
    let n = bytes.len().min(out.len());
    out[..n].copy_from_slice(&bytes[..n]);
    Ok(bytes.len())
}
