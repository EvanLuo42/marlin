//! The command line and executable path.

use core::ffi::{c_char, c_int};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::Result;
use crate::backend::unix::last_error;
use crate::backend::unix::process::copy_utf8;

static ARGC: AtomicUsize = AtomicUsize::new(0);
static ARGV: AtomicPtr<*const c_char> = AtomicPtr::new(core::ptr::null_mut());

/// glibc calls `.init_array` functions with `argc`, `argv` and `envp` before `main` (musl does
/// not), so the arguments are known without a C `main`. Rust std relies on the same.
#[used]
#[unsafe(link_section = ".init_array.00099")]
static ARGV_INIT: extern "C" fn(c_int, *const *const c_char, *const *const c_char) = capture;

extern "C" fn capture(argc: c_int, argv: *const *const c_char, _envp: *const *const c_char) {
    ARGV.store(argv.cast_mut(), Ordering::Relaxed);
    ARGC.store(argc.max(0) as usize, Ordering::Relaxed);
}

/// The process arguments; initializers run before any other thread exists.
pub(crate) fn argv() -> (usize, *const *const c_char) {
    (
        ARGC.load(Ordering::Relaxed),
        ARGV.load(Ordering::Relaxed).cast_const(),
    )
}

pub(crate) fn current_exe(out: &mut [u8]) -> Result<usize> {
    let mut buf = [0u8; crate::file::MAX_PATH_LEN * 4];
    // SAFETY: the path is NUL-terminated; `buf` is valid for `buf.len()` bytes.
    let n = unsafe {
        libc::readlink(
            c"/proc/self/exe".as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        return Err(last_error());
    }
    // readlink truncates silently; a full buffer may be a truncated path.
    if n as usize == buf.len() {
        return Err(crate::Error::from_kind(crate::ErrorKind::InvalidInput));
    }
    copy_utf8(&buf[..n as usize], out)
}
