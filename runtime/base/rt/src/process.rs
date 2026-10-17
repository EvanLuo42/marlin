//! The process: command line, environment, executable and working directory, exit.
//!
//! The safe, allocating layer over `marlin_pal::process`. Arguments come from the OS, not from a
//! C `main` (Unicode on Windows, where the C runtime's `argv` is in the ANSI code page).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use marlin_pal::process as pal;
pub use marlin_pal::{Error, ErrorKind, Result};

/// Reads a string the PAL writes into a caller buffer, growing the buffer until it fits.
fn read(mut f: impl FnMut(&mut [u8]) -> Result<Option<usize>>) -> Result<Option<String>> {
    let mut buf = vec![0u8; 256];
    loop {
        let Some(len) = f(&mut buf)? else {
            return Ok(None);
        };
        if len <= buf.len() {
            buf.truncate(len);
            // The PAL checked it is UTF-8.
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|_| Error::from_kind(ErrorKind::InvalidData));
        }
        buf.resize(len, 0);
    }
}

static ARGS: AtomicPtr<&'static str> = AtomicPtr::new(core::ptr::null_mut());
static ARGS_LEN: AtomicUsize = AtomicUsize::new(0);

/// All command-line arguments, the program name first, read once and kept for the life of the
/// process. Arguments that are not valid Unicode are left out.
pub fn all_args() -> &'static [&'static str] {
    let ptr = ARGS.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: published below with Release after being fully written, and never freed.
        return unsafe { core::slice::from_raw_parts(ptr, ARGS_LEN.load(Ordering::Relaxed)) };
    }
    let args: Vec<&'static str> = (0..pal::arg_count())
        .filter_map(|i| read(|buf| pal::arg(i, buf)).ok().flatten())
        .map(|s| &*Box::leak(s.into_boxed_str()))
        .collect();
    let args = Box::leak(args.into_boxed_slice());
    ARGS_LEN.store(args.len(), Ordering::Relaxed);
    // Racing first calls each leak a copy; one wins and the rest are small, one-time leaks.
    match ARGS.compare_exchange(
        core::ptr::null_mut(),
        args.as_mut_ptr(),
        Ordering::Release,
        Ordering::Acquire,
    ) {
        Ok(_) => args,
        // SAFETY: as above.
        Err(winner) => unsafe {
            core::slice::from_raw_parts(winner, ARGS_LEN.load(Ordering::Relaxed))
        },
    }
}

/// The command-line arguments after the program name.
pub fn args() -> impl Iterator<Item = &'static str> {
    all_args().iter().skip(1).copied()
}

/// The value of environment variable `name`, or `None` if it is not set or not valid Unicode.
pub fn env_var(name: &str) -> Option<String> {
    read(|buf| pal::env_var(name, buf)).ok().flatten()
}

/// The path of the running executable, in the OS's form.
pub fn current_exe() -> Result<String> {
    read(|buf| pal::current_exe(buf).map(Some)).map(Option::unwrap_or_default)
}

/// The current working directory, in the OS's form.
pub fn current_dir() -> Result<String> {
    read(|buf| pal::current_dir(buf).map(Some)).map(Option::unwrap_or_default)
}

/// Changes the process-wide working directory.
pub fn set_current_dir(path: &str) -> Result<()> {
    pal::set_current_dir(path)
}

/// The OS process id.
#[inline]
pub fn id() -> u32 {
    pal::id()
}

/// Flushes the log sinks and ends the process with `code`. Destructors do not run.
pub fn exit(code: i32) -> ! {
    marlin_diag::flush();
    pal::exit(code)
}
