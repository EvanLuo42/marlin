//! Debug output, debugger interaction, stack capture and symbols.

use core::fmt;

use crate::backend::imp::Backend;
use crate::contract::Debug;

/// Writes `s` to stderr and, when a debugger is attached, to the debugger.
///
/// Never allocates or takes locks, so it is safe to call while panicking.
#[inline]
pub fn output(s: &str) {
    Backend::output(s)
}

#[inline]
pub fn is_debugger_present() -> bool {
    Backend::is_debugger_present()
}

/// Raises a breakpoint trap. Without a debugger this usually terminates the process.
#[inline]
pub fn break_point() {
    Backend::break_point()
}

/// Makes stderr show ANSI color escapes if it can, and reports whether it does: on Windows,
/// enables virtual terminal processing on a console (Windows 10+); on POSIX, checks that stderr is
/// a terminal. False when stderr is a file or pipe.
#[inline]
pub fn enable_terminal_colors() -> bool {
    Backend::enable_terminal_colors()
}

/// Writes the return addresses of the calling thread's stack into `out`, innermost first, and
/// returns how many were written. The innermost `skip` frames of the caller are left out
/// (`capture_stack` itself never appears).
///
/// Windows: `RtlCaptureStackBackTrace` (unwind data, frame pointers not needed). POSIX:
/// `backtrace`, which uses the unwind tables Rust emits; glibc loads libgcc_s on the first call,
/// so capture once at startup if the first capture may happen in a crash.
#[inline(never)]
pub fn capture_stack(skip: usize, out: &mut [usize]) -> usize {
    Backend::capture_stack(skip, out)
}

/// A loaded executable or shared library.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Module {
    /// Address the module is loaded at; `addr - base` is what offline symbolizers take.
    pub base: usize,
    /// Full length of the module's path; the buffer holds it when this fits.
    pub path_len: usize,
}

/// What [`symbolize`] found for an address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Symbol {
    /// Full length of the (possibly mangled) function name; the buffer holds it when this fits.
    pub name_len: usize,
    /// `addr` minus the function's start.
    pub offset: usize,
    /// Full length of the source file path, 0 when unknown.
    pub file_len: usize,
    /// Source line, 0 when unknown.
    pub line: u32,
}

/// The module containing `addr`, writing its path into `path`; `None` if no module does.
#[inline]
pub fn module_of(addr: usize, path: &mut [u8]) -> Option<Module> {
    Backend::module_of(addr, path)
}

/// Best-effort symbol lookup for a code address, writing the function name into `name` and the
/// source file into `file`. `None` if nothing is known.
///
/// Windows reads PDBs through dbghelp (names are demangled, lines included; serialized
/// internally, and slow on first use per module). POSIX uses `dladdr`, which only sees exported
/// dynamic symbols, mangled and without lines: symbolize offline from [`module_of`] offsets for
/// the rest.
#[inline]
pub fn symbolize(addr: usize, name: &mut [u8], file: &mut [u8]) -> Option<Symbol> {
    Backend::symbolize(addr, name, file)
}

/// Formats into a fixed buffer and hands it to [`output`] when full, on [`flush`](Self::flush) or
/// on drop, so a formatted line costs one call instead of one per fragment. Never allocates.
pub struct OutputBuffer {
    buf: [u8; 512],
    len: usize,
}

impl OutputBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }

    pub fn flush(&mut self) {
        if self.len > 0 {
            // SAFETY: the buffer only ever holds whole `&str`s, so it is valid UTF-8.
            output(unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) });
            self.len = 0;
        }
    }
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for OutputBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.len + s.len() > self.buf.len() {
            self.flush();
        }
        if s.len() > self.buf.len() {
            output(s);
        } else {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
        }
        Ok(())
    }
}

impl Drop for OutputBuffer {
    fn drop(&mut self) {
        self.flush();
    }
}
